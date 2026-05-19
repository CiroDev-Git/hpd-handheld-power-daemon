mod probe;

use tracing::{error, info, Level};
use tracing_subscriber::FmtSubscriber;
use tokio::sync::mpsc;
use tokio::signal;

use hpd_sysfs::RealSysfs;
use hpd_capabilities::power::PowerEnvelopeLimits;
use hpd_capabilities::profile::ProfileThresholds;
use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::units::PowerMilliwatts;
use hpd_capabilities::profile::ProfileName;
use hpd_core::state::ProfileState;
use hpd_core::transition::Transition;
use hpd_core::executor::Executor;
use hpd_core::persistence::StatePersister;

use hpd_backend_asus::detect::matches_asus_handheld;
use hpd_backend_asus::power::AsusPowerBackend;

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
    let sysfs = RealSysfs::new();

    // 4. Choose L1 (Backend) based on detection
    if let Some(asus_model) = matches_asus_handheld(&dmi) {
        info!("ASUS handheld detected: {:?}", asus_model);
        run_daemon(AsusPowerBackend::new(sysfs)).await?;
    } else {
        error!("Hardware not supported or recognized. Exiting gracefully.");
        std::process::exit(1);
    }

    Ok(())
}

async fn run_daemon<B>(backend: B) -> Result<(), Box<dyn std::error::Error>>
where 
    // As far, force PowerEnvelope implementation only (after will use complete HwBackend)
    B: hpd_capabilities::power::PowerEnvelope + Send + Sync + 'static 
{
    // 5. Base config
    let limits = PowerEnvelopeLimits {
        spl_min: PowerMilliwatts(7000),
        spl_max: PowerMilliwatts(35000), // Ally X range
    };
    let thresholds = ProfileThresholds { low_frac: 0.33, high_frac: 0.67 };
    let persister = StatePersister::new("/var/tmp/hpd_state.toml"); // Using /tmp temporally for testing

    // Initial state (after will read from persister)
    let initial_state = ProfileState {
        power_target: PowerEnvelopeTarget {
            spl: PowerMilliwatts(15000),
            sppt: PowerMilliwatts(15000),
            fppt: Some(PowerMilliwatts(15000)),
        },
        active_profile: ProfileName::Balanced,
        charge_end_threshold: 80,
        fan_follows_tdp: true,
        is_ac_connected: true,
    };

    // 6. Create communication channels
    let (tx, rx) = mpsc::channel::<Transition>(32);
    let internal_tx = tx.clone(); // For rollback

    // 7. Executor instance
    let (executor, _state_rx) = Executor::new(
        backend,
        initial_state,
        limits,
        thresholds,
        rx,
        internal_tx,
        persister,
    );

    // 8. Run async engine
    info!("Spawning main executor loop...");
    let executor_handle = tokio::spawn(async move {
        executor.run().await;
    });


    // 9. Start D-Bus server
    info!("Starting D-Bus server on session bus...");
    let dbus_interface = hpd_dbus::service::PowerDaemonInterface::new(tx.clone(), state_rx);

    let _conn = zbus::ConnectionBuilder::session()?
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
}