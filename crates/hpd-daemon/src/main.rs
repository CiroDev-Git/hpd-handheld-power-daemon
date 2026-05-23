mod probe;
mod suspend;

use tracing::{error, info};
use tracing_subscriber::{EnvFilter, FmtSubscriber};
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
    // 1. Init logging (journald/terminal). Respects RUST_LOG; defaults to `hpd=info,warn`.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("hpd=info,warn"));
    let subscriber = FmtSubscriber::builder()
        .with_env_filter(filter)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("Starting Handheld Power Daemon (hpd)...");

    // 2. Detect hardware
    let dmi = probe::read_system_dmi();
    info!(vendor = %dmi.board_vendor, board = %dmi.board_name, "Hardware detected");

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

            // Inject the mock instead of the real system.
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
            info!(
                spl_min_w = l.spl_min.0 / 1000,
                spl_max_w = l.spl_max.0 / 1000,
                "Hardware limits detected"
            );
            l
        },
        Err(e) => {
            error!("CRITICAL: Cannot read hardware limits: {}. Exiting.", e);
            return Err(e.into());
        }
    };
    let thresholds = ProfileThresholds { low_frac: 0.33, high_frac: 0.67 };
    let persister = StatePersister::new("/var/tmp/hpd_state.toml"); // FIXME(Lote 7): move to /var/lib/hpd/state.toml

    let is_physically_plugged = backend.is_ac_connected().unwrap_or(false);

    let initial_state = match persister.load().await {
        Some(mut state) => {
            info!("Previous state loaded successfully from disk.");
            state.is_ac_connected = is_physically_plugged;
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
                fan_follows_tdp: true,
                is_ac_connected: is_physically_plugged,
                last_dc_target: None
            }
        }
    };

    // 6. Create communication channels
    let (tx, rx) = mpsc::channel::<Transition>(32);
    let internal_tx = tx.clone(); // For rollback

    info!("Starting hardware event monitors...");
    let tx_netlink = tx.clone(); // Give to monitor their own remote control
    // 1. Use a native OS thread so the netlink monitor never blocks the main pool.
    std::thread::spawn(move || {
        // 2. Single-thread async runtime pinned to this thread.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to build local tokio runtime for netlink");

        // 3. Run the !Send netlink task safely on a LocalSet.
        rt.block_on(async move {
            let local = tokio::task::LocalSet::new();
            local.run_until(async move {
                hpd_netlink::spawn_power_monitor(tx_netlink).await;
            }).await;
        });
    });

    info!("Starting sleep detection...");
    let tx_suspend = tx.clone();
    tokio::spawn(async move {
        suspend::spawn_suspend_monitor(tx_suspend).await;
    });

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

    // 10. Wait for shutdown signal (Ctrl+C or SIGTERM from systemd).
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