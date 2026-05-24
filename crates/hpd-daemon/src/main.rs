// SPDX-License-Identifier: GPL-3.0-or-later

#![cfg_attr(
    not(test),
    warn(clippy::unwrap_used, clippy::expect_used, clippy::panic)
)]

// Daemon sub-modules are only reachable through `run_daemon`, which is
// only compiled in when at least one vendor backend is active. Without
// a vendor feature the binary still builds (CI verifies this) but
// every entry point below `main` is dead code — gate them so the
// build stays warning-free under `--no-default-features`.
#[cfg(feature = "vendor-asus")]
mod config;
#[cfg(feature = "vendor-asus")]
mod probe;
#[cfg(feature = "vendor-asus")]
mod suspend;

use tracing::error;
use tracing_subscriber::{EnvFilter, FmtSubscriber};

#[cfg(feature = "vendor-asus")]
use std::path::PathBuf;

#[cfg(feature = "vendor-asus")]
use tokio::signal;
#[cfg(feature = "vendor-asus")]
use tokio::signal::unix::SignalKind;
#[cfg(feature = "vendor-asus")]
use tokio::sync::mpsc;
#[cfg(feature = "vendor-asus")]
use tracing::{info, warn};

#[cfg(feature = "vendor-asus")]
use hpd_capabilities::power::PowerEnvelopeTarget;
#[cfg(feature = "vendor-asus")]
use hpd_capabilities::profile::ProfileName;
#[cfg(feature = "vendor-asus")]
use hpd_core::executor::Executor;
#[cfg(feature = "vendor-asus")]
use hpd_core::persistence::StatePersister;
#[cfg(feature = "vendor-asus")]
use hpd_core::state::ProfileState;
#[cfg(feature = "vendor-asus")]
use hpd_core::transition::Transition;
#[cfg(feature = "vendor-asus")]
use hpd_sysfs::RealSysfs;

#[cfg(feature = "vendor-asus")]
use crate::config::DaemonConfig;

#[cfg(feature = "vendor-asus")]
use hpd_backend_asus::detect::matches_asus_handheld;
#[cfg(feature = "vendor-asus")]
use hpd_backend_asus::AsusBackend;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Init logging unconditionally so the "no vendor compiled in" build
    // still prints something useful before exiting. Respects RUST_LOG;
    // defaults to `hpd=info,warn`.
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("hpd=info,warn"));
    let subscriber = FmtSubscriber::builder().with_env_filter(filter).finish();
    tracing::subscriber::set_global_default(subscriber)?;

    // No vendor backend compiled in → log and exit cleanly. Build with
    // `--features vendor-asus` (or just the default feature set) for a
    // working daemon. Exit code 2 distinguishes "misconfigured build"
    // from "hardware not supported" (exit 1).
    #[cfg(not(feature = "vendor-asus"))]
    {
        error!("No vendor backend compiled in (build with --features vendor-asus). Exiting.");
        std::process::exit(2);
    }

    #[cfg(feature = "vendor-asus")]
    {
        run_real_main().await
    }
}

#[cfg(feature = "vendor-asus")]
async fn run_real_main() -> Result<(), Box<dyn std::error::Error>> {
    info!("Starting Handheld Power Daemon (hpd)...");

    // Detect hardware.
    let dmi = probe::read_system_dmi();
    info!(vendor = %dmi.board_vendor, board = %dmi.board_name, "Hardware detected");

    // Simulator mode (compiled in only with `--features simulator`).
    #[cfg(feature = "simulator")]
    {
        if std::env::var("HPD_SIMULATOR").is_ok() {
            info!("Starting in SIMULATOR mode...");
            let mock = hpd_sysfs::MockSysfs::new();

            mock.create_file(
                "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/min_value",
                "7",
            );
            mock.create_file(
                "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/max_value",
                "35",
            );
            mock.create_file(
                "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value",
                "15",
            );
            mock.create_file(
                "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/max_value",
                "43",
            );
            mock.create_file(
                "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value",
                "15",
            );
            mock.create_file(
                "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl3_fppt/max_value",
                "55",
            );
            mock.create_file(
                "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl3_fppt/current_value",
                "15",
            );
            mock.create_file("sys/firmware/acpi/platform_profile", "balanced");
            mock.create_file(
                "sys/firmware/acpi/platform_profile_choices",
                "quiet balanced performance",
            );
            mock.create_file(
                "sys/class/power_supply/BAT0/charge_control_end_threshold",
                "80",
            );

            // Simulator only models ASUS firmware (enforced via
            // simulator → vendor-asus in Cargo.toml).
            run_daemon(AsusBackend::new(mock)).await?;
            return Ok(());
        }
    }

    // Production mode: pick the L1 backend by DMI detection.
    if let Some(asus_model) = matches_asus_handheld(&dmi) {
        info!("ASUS handheld detected: {:?}", asus_model);
        run_daemon(AsusBackend::new(RealSysfs::new())).await?;
        return Ok(());
    }

    error!("Hardware not supported or recognized (no vendor backend matched). Exiting gracefully.");
    std::process::exit(1);
}

#[cfg(feature = "vendor-asus")]
async fn run_daemon<B>(backend: B) -> Result<(), Box<dyn std::error::Error>>
where
    B: hpd_capabilities::backend::HwBackend + 'static,
{
    // 5. Daemon configuration. `ConfigurationDirectory=hpd` in the unit
    //    file points us at /etc/hpd. Outside systemd we fall back to
    //    /etc/hpd/config.toml directly. Missing or corrupt → defaults.
    let config_path = std::env::var("CONFIGURATION_DIRECTORY")
        .map(|d| PathBuf::from(d).join("config.toml"))
        .unwrap_or_else(|_| PathBuf::from("/etc/hpd/config.toml"));
    let daemon_config = DaemonConfig::load(&config_path);

    let limits = match backend.get_limits() {
        Ok(l) => {
            info!(
                spl_min_w = l.spl_min.as_watts(),
                spl_max_w = l.spl_max.as_watts(),
                "Hardware limits detected"
            );
            l
        }
        Err(e) => {
            error!("CRITICAL: Cannot read hardware limits: {}. Exiting.", e);
            return Err(e.into());
        }
    };

    // systemd's StateDirectory= injects STATE_DIRECTORY (e.g. /var/lib/hpd).
    // Outside systemd we honour the config's `state_path`. /var/tmp is
    // intentionally avoided: world-writable + survives reboots = symlink-race
    // surface when running as root.
    let state_path = std::env::var("STATE_DIRECTORY")
        .map(|d| PathBuf::from(d).join("state.toml"))
        .unwrap_or_else(|_| daemon_config.state_path.clone());
    info!(path = %state_path.display(), "Using state file");
    let persister = StatePersister::new(state_path);

    let is_physically_plugged = backend.is_ac_connected().unwrap_or(false);

    let initial_state = match persister.load().await {
        Some(mut state) => {
            info!("Previous state loaded successfully from disk.");
            state.is_ac_connected = is_physically_plugged;
            state
        }
        None => {
            info!("No previous state found (or failed to read). Defaulting to hardware values...");
            // First time after installation, read currentconfig of device
            let current_target = backend.get_target().unwrap_or(PowerEnvelopeTarget {
                spl: limits.spl_min,
                sppt: limits.spl_min,
                fppt: Some(limits.spl_min),
            });
            let current_profile = backend
                .get_active_profile()
                .unwrap_or(ProfileName::Balanced);
            let current_charge_limit = backend
                .get_end_threshold()
                .unwrap_or(daemon_config.default_charge_threshold);

            ProfileState {
                power_target: current_target,
                active_profile: current_profile,
                charge_end_threshold: current_charge_limit,
                fan_follows_tdp: true,
                is_ac_connected: is_physically_plugged,
                last_dc_target: None,
            }
        }
    };

    // 6. Create communication channels
    let (tx, rx) = mpsc::channel::<Transition>(daemon_config.channel_capacity);
    let internal_tx = tx.clone(); // For rollback

    // SIGHUP reloads the config file and pushes a ConfigReload transition.
    // Maps cleanly to `systemctl reload hpd` via `ExecReload=` in the unit.
    let tx_reload = tx.clone();
    let path_reload = config_path.clone();
    tokio::spawn(async move {
        let mut stream = match tokio::signal::unix::signal(SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "Cannot register SIGHUP handler; config reload disabled");
                return;
            }
        };
        info!("SIGHUP handler ready — `systemctl reload hpd` or `kill -HUP <pid>` reloads config");
        while stream.recv().await.is_some() {
            let new_cfg = DaemonConfig::load(&path_reload);
            if tx_reload
                .send(Transition::ConfigReload(new_cfg.to_runtime()))
                .await
                .is_err()
            {
                warn!("Executor channel closed; SIGHUP handler exiting");
                return;
            }
        }
    });

    info!("Starting hardware event monitors...");
    let tx_netlink = tx.clone(); // Give to monitor their own remote control
                                 // 1. Use a native OS thread so the netlink monitor never blocks the main pool.
    std::thread::spawn(move || {
        // 2. Single-thread async runtime pinned to this thread. Construction
        //    can only fail under pathological system state (no fds, no
        //    epoll); if that happens the netlink monitor is dead in the
        //    water — log and exit the thread instead of panicking, the
        //    main daemon keeps running without AC plug events.
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                error!(error = %e, "Failed to build netlink runtime; AC plug events will be missed");
                return;
            }
        };

        // 3. Run the !Send netlink task safely on a LocalSet.
        rt.block_on(async move {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async move {
                    hpd_netlink::spawn_power_monitor(tx_netlink).await;
                })
                .await;
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
        daemon_config.to_runtime(),
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
    info!("Starting D-Bus server...");
    const DBUS_BUS_NAME: &str = "dev.cirodev.hpd.PowerDaemon1";
    const DBUS_OBJECT_PATH: &str = "/dev/cirodev/hpd/PowerDaemon1";

    // Clone the state receiver: one copy lives inside the interface (for
    // synchronous property reads), the other drives the PropertiesChanged
    // emitter task spawned below.
    let state_rx_for_watcher = state_rx.clone();

    let dbus_interface = hpd_dbus::service::PowerDaemonInterface::new(tx.clone(), state_rx, limits);

    // Session bus is only a valid target when the simulator path is
    // compiled in; production builds always bind to the system bus
    // regardless of HPD_SIMULATOR being set in the environment.
    #[cfg(feature = "simulator")]
    let use_session_bus = std::env::var("HPD_SIMULATOR").is_ok();
    #[cfg(not(feature = "simulator"))]
    let use_session_bus = false;

    let conn_builder = if use_session_bus {
        info!("Using Session Bus (Simulator Mode)");
        zbus::ConnectionBuilder::session()?
    } else {
        info!("Using System Bus (Production Mode)");
        zbus::ConnectionBuilder::system()?
    };

    let conn = conn_builder
        .name(DBUS_BUS_NAME)?
        .serve_at(DBUS_OBJECT_PATH, dbus_interface)?
        .build()
        .await?;

    // Spawn the PropertiesChanged emitter. Each state mutation in the
    // executor publishes a new ProfileState on the watch channel; we
    // observe it and call the zbus-generated `<prop>_changed` notifiers
    // for the properties whose underlying field actually changed. This is
    // the real wiring behind §3.1 of AUDIT_V1; the previous "implicit"
    // approach was a no-op for external D-Bus clients.
    let iface_ref = conn
        .object_server()
        .interface::<_, hpd_dbus::service::PowerDaemonInterface>(DBUS_OBJECT_PATH)
        .await?;
    tokio::spawn(spawn_properties_changed_emitter(
        state_rx_for_watcher,
        iface_ref,
    ));

    info!("Daemon is fully running and listening for commands.");

    // 10. Wait for shutdown signal (Ctrl+C from a terminal, SIGTERM from
    //     systemd) and trigger a graceful drain. SIGTERM registration only
    //     fails on system-level breakage (no epoll, exhausted fds); if
    //     that happens at startup the daemon can't reliably do its job, so
    //     we propagate the error rather than degrade silently.
    let mut sigterm = tokio::signal::unix::signal(SignalKind::terminate())?;
    tokio::select! {
        result = signal::ctrl_c() => {
            match result {
                Ok(()) => info!("Received SIGINT, shutting down gracefully..."),
                Err(e) => error!(error = %e, "SIGINT handler failed"),
            }
        }
        _ = sigterm.recv() => info!("Received SIGTERM, shutting down gracefully..."),
    }

    // 11. Drain: tell the executor to persist state and exit.
    if tx.send(Transition::Shutdown).await.is_err() {
        warn!("Executor channel already closed before Shutdown could be sent");
    }
    // 5s is a belt-and-suspenders bound below systemd's 90s default
    // TimeoutStopSec — if persistence hangs, log and move on rather than
    // letting systemd SIGKILL us mid-write.
    match tokio::time::timeout(std::time::Duration::from_secs(5), executor_handle).await {
        Ok(Ok(())) => info!("Executor drained cleanly"),
        Ok(Err(e)) => error!(error = %e, "Executor task panicked during shutdown"),
        Err(_) => warn!("Executor did not exit within 5s; abandoning"),
    }

    // 12. Close the D-Bus connection so registered names are released
    //     immediately instead of waiting for runtime teardown.
    if let Err(e) = conn.close().await {
        warn!(error = %e, "Closing D-Bus connection failed");
    }

    info!("Shutdown complete");
    Ok(())
}

/// Watch the executor's state channel and emit zbus `PropertiesChanged`
/// signals for the D-Bus properties whose underlying field changed.
///
/// The task exits cleanly when the executor drops its state sender (i.e.
/// during daemon shutdown), at which point `changed()` returns `Err`.
#[cfg(feature = "vendor-asus")]
async fn spawn_properties_changed_emitter(
    mut state_rx: tokio::sync::watch::Receiver<ProfileState>,
    iface_ref: zbus::InterfaceRef<hpd_dbus::service::PowerDaemonInterface>,
) {
    // Snapshot the initial state without holding the borrow across the await.
    let mut last = state_rx.borrow().clone();

    loop {
        if state_rx.changed().await.is_err() {
            info!("State channel closed, stopping D-Bus properties watcher");
            return;
        }

        // borrow_and_update marks the value as seen so the next `changed()`
        // fires only for newer mutations.
        let new = state_rx.borrow_and_update().clone();

        let ctx = iface_ref.signal_context();
        let iface = iface_ref.get().await;

        if new.power_target.spl != last.power_target.spl {
            if let Err(e) = iface.current_spl_changed(ctx).await {
                error!(error = %e, "Failed to emit current_spl PropertiesChanged");
            }
        }
        if new.active_profile != last.active_profile {
            if let Err(e) = iface.active_profile_changed(ctx).await {
                error!(error = %e, "Failed to emit active_profile PropertiesChanged");
            }
        }
        if new.charge_end_threshold != last.charge_end_threshold {
            if let Err(e) = iface.charge_end_threshold_changed(ctx).await {
                error!(error = %e, "Failed to emit charge_end_threshold PropertiesChanged");
            }
        }

        last = new;
    }
}
