mod probe;

use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;
use tokio::sync::mpsc;
use tokio::signal;

use hpd_sysfs::RealSysfs;
use hpd_capabilities::profile::ProfileThresholds;
use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::ProfileName;
use hpd_core::state::ProfileState;
use hpd_core::transition::Transition;
use hpd_core::executor::Executor;
use hpd_core::persistence::StatePersister;

use hpd_backend_asus::detect::matches_asus_handheld;
use hpd_backend_asus::AsusBackend;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Init logs system (journald/terminal)
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("Starting Handheld Power Daemon (hpd)...");

    // 2. Detect hardware
    let dmi = probe::read_system_dmi();
    info!("Hardware detected: {} - {}", dmi.board_vendor, dmi.board_name);

    // 3. Init L0 (I/O Real)

    // --- Simulator mode for macOS ---
    if std::env::var("HPD_SIMULATOR").is_ok() {
        info!("Starting in SIMULATOR mode...");
        {
            let mock = hpd_sysfs::MockSysfs::new();
            
            mock.create_file("sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/min_value", "7");
            mock.create_file("sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/max_value", "35");
            mock.create_file("sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value", "15");
            mock.create_file("sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value", "15");
            mock.create_file("sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl3_fppt/current_value", "15");
            mock.create_file("sys/firmware/acpi/platform_profile", "balanced");
            mock.create_file("sys/firmware/acpi/platform_profile_choices", "quiet balanced performance");
            mock.create_file("sys/class/power_supply/BAT0/charge_control_end_threshold", "80");
            
            // Inyectamos el Mock en lugar del sistema real!
            run_daemon(AsusBackend::new(mock)).await?;
            return Ok(());
        }
    }
    // ---------------------------------

    // --- Production mode ---
    let sysfs = RealSysfs::new();

    // 4. Choose L1 (Backend) based on detection
    if let Some(asus_model) = matches_asus_handheld(&dmi) {
        info!("ASUS handheld detected: {:?}", asus_model);
        run_daemon(AsusBackend::new(sysfs)).await?;
    } else {
        error!("Hardware not supported or recognized. Exiting gracefully.");
        std::process::exit(1);
    }

    Ok(())
}

async fn run_daemon<B>(backend: B) -> Result<(), Box<dyn std::error::Error>>
where 
    B: hpd_capabilities::backend::HwBackend + 'static 
{
    // 5. Base config
    let limits = match backend.get_limits() {
        Ok(l) => {
            info!("Hardware limits detected: {}W to {}W", l.spl_min.0 / 1000, l.spl_max.0 / 1000);
            l
        },
        Err(e) => {
            error!("CRITICAL: Cannot read hardware limits: {}. Exiting.", e);
            return Err(e.into());
        }
    };
    let thresholds = ProfileThresholds { low_frac: 0.33, high_frac: 0.67 };
    let persister = StatePersister::new("/var/tmp/hpd_state.toml"); // Using /tmp temporally for testing

    let initial_state = match persister.load().await {
        Some(state) => {
            info!("Previous state loaded successfully from disk.");
            state
        },
        None => {
            info!("No previous state found (or failed to read). Defaulting to hardware values...");
            // First time after installation, read currentconfig of device
            let current_target = backend.get_target().unwrap_or(PowerEnvelopeTarget {
                spl: limits.spl_min, 
                sppt: limits.spl_min, 
                fppt: Some(limits.spl_min)
            });
            let current_profile = backend.get_active_profile().unwrap_or(ProfileName::Balanced);
            let current_charge_limit = backend.get_end_threshold().unwrap_or(80);

            ProfileState {
                power_target: current_target,
                active_profile: current_profile,
                charge_end_threshold: current_charge_limit,
                // 'true' as Factory Default in the first time
                fan_follows_tdp: true,
                // Read UPower o /sys/class/power_supply/AC
                is_ac_connected: true,
            }
        }
    };

    // 6. Create communication channels
    let (tx, rx) = mpsc::channel::<Transition>(32);
    let internal_tx = tx.clone(); // For rollback

    // 7. Executor instance
    let (executor, state_rx) = Executor::new(
        backend,
        initial_state,
        limits.clone(),
        thresholds,
        rx,
        internal_tx,
        persister,
    );

    // 8. Run async engine
    info!("Spawning main executor loop...");
    let _executor_handle = tokio::spawn(async move {
        executor.run().await;
    });


    // 9. Start D-Bus server
    info!("Starting D-Bus server...");
    let dbus_interface = hpd_dbus::service::PowerDaemonInterface::new(
        tx.clone(), 
        state_rx, 
        limits
    );

    let conn_builder = if std::env::var("HPD_SIMULATOR").is_ok() {
        info!("Using Session Bus (Simulator Mode)");
        zbus::ConnectionBuilder::session()?
    } else {
        info!("Using System Bus (Production Mode)");
        zbus::ConnectionBuilder::system()?
    };
    
    let _conn = conn_builder
        .name("dev.cirodev.hpd.PowerDaemon1")?
        .serve_at("/dev/cirodev/hpd/PowerDaemon1", dbus_interface)?
        .build()
        .await?;

    info!("Daemon is fully running and listening for commands.");

    // 10. Wait until turn off signal (Ctrl+C o SIGTERM de systemd)
    match signal::ctrl_c().await {
        Ok(()) => {
            info!("Received shutdown signal. Shutting down gracefully...");
        },
        Err(err) => {
            error!("Unable to listen for shutdown signal: {}", err);
        },
    }

    Ok(())
}