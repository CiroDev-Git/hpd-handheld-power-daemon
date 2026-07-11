// SPDX-License-Identifier: GPL-3.0-or-later

//! `hpd-daemon` — the long-running root service.
//!
//! Composition root for the Handheld Power Daemon: detects the
//! hardware via DMI, picks an L1 backend, loads `/etc/hpd/config.toml`,
//! wires the [`hpd_core::executor::Executor`] to the D-Bus
//! interface and the netlink/suspend monitors, and drives the
//! lifecycle (SIGHUP reload, SIGINT/SIGTERM graceful drain).
//!
//! Publishes `dev.cirodev.hpd.PowerDaemon1` on the system bus in
//! production and on the session bus when built with
//! `--features simulator`. See the project's `CLAUDE.md` for the
//! end-to-end wiring map and the concurrency layout.

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
use hpd_capabilities::fan_curve::FanCurveSelection;
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

/// One-screen help shown by `hpd-daemon --help`. The daemon is normally
/// started by systemd, not by hand; this exists so a user who runs it
/// directly to "see what it does" gets oriented instead of accidentally
/// launching a service in the foreground.
const DAEMON_HELP: &str = "\
hpd-daemon — Handheld Power Daemon (the long-running root service).

Manages TDP / power envelope, ACPI cooling profile, fan reporting, and
battery charge thresholds on supported handheld PCs. Exposes the D-Bus
interface dev.cirodev.hpd.PowerDaemon1 on the system bus.

You normally do NOT run this by hand — it is started by systemd:

  sudo systemctl enable --now hpd     Start the service now and at boot
  systemctl status hpd                Check whether it is running
  journalctl -fu hpd                  Follow the daemon's live logs
  sudo systemctl reload hpd           Re-read /etc/hpd/config.toml (SIGHUP)

To change power, cooling, or battery settings, use the CLI instead:

  hpdctl --help                       Show the user-facing commands
  hpdctl status                       Current TDP / profile / battery state

Configuration: /etc/hpd/config.toml   (see /etc/hpd/config.toml.example)
Persisted state: /var/lib/hpd/state.toml

Environment:
  RUST_LOG        Override log filter (default: hpd=info,warn)
  HPD_SIMULATOR   Run against a mock backend on the session bus
                  (only with a binary built --features simulator)

Options:
  -h, --help      Print this help and exit
  -V, --version   Print version and exit";

/// Handle `--help` / `--version` before doing anything else, and exit.
/// Returns normally for the daemon's usual no-argument invocation.
fn handle_cli_args() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{DAEMON_HELP}");
        std::process::exit(0);
    }
    if args.iter().any(|a| a == "-V" || a == "--version") {
        println!("hpd-daemon {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }
    if let Some(unknown) = args.first() {
        eprintln!("hpd-daemon: unrecognized argument '{unknown}'\n");
        eprintln!("{DAEMON_HELP}");
        std::process::exit(2);
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    handle_cli_args();

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

            // Fan telemetry node (`asus` hwmon) — the RPM read path
            // resolves it by name, so the index is arbitrary.
            mock.create_file("sys/class/hwmon/hwmon0/name", "asus");
            mock.create_file("sys/class/hwmon/hwmon0/fan1_input", "3200");
            mock.create_file("sys/class/hwmon/hwmon0/fan2_input", "3000");
            // Decoy `acpi_fan` node that also exposes a fan1_input — the
            // backend must skip it and read the `asus` node above.
            mock.create_file("sys/class/hwmon/hwmon3/name", "acpi_fan");
            mock.create_file("sys/class/hwmon/hwmon3/fan1_input", "9999");
            // Temperature sensors: k10temp (CPU Tctl) + amdgpu (GPU edge).
            mock.create_file("sys/class/hwmon/hwmon6/name", "k10temp");
            mock.create_file("sys/class/hwmon/hwmon6/temp1_input", "61000");
            mock.create_file("sys/class/hwmon/hwmon5/name", "amdgpu");
            mock.create_file("sys/class/hwmon/hwmon5/temp1_input", "54000");
            mock.create_file("sys/class/hwmon/hwmon5/power1_input", "16088000"); // 16.1 W
                                                                                 // Custom fan-curve node (`asus_custom_fan_curve` hwmon),
                                                                                 // seeded with the firmware default curve and auto mode.
            mock.create_file("sys/class/hwmon/hwmon1/name", "asus_custom_fan_curve");
            for fan in [1u8, 2] {
                mock.create_file(format!("sys/class/hwmon/hwmon1/pwm{fan}_enable"), "2");
                for point in 1..=8u8 {
                    mock.create_file(
                        format!("sys/class/hwmon/hwmon1/pwm{fan}_auto_point{point}_temp"),
                        "0",
                    );
                    mock.create_file(
                        format!("sys/class/hwmon/hwmon1/pwm{fan}_auto_point{point}_pwm"),
                        "0",
                    );
                }
            }

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

    let limits = match backend.power().get_limits() {
        Ok(l) => {
            info!(
                spl_min_w = l.spl_min.as_watts(),
                spl_max_w = l.spl_max.as_watts(),
                "Hardware limits detected"
            );
            l
        }
        Err(e) => {
            error!(
                error = %e,
                "CRITICAL: Cannot read hardware power limits from \
                 /sys/class/firmware-attributes/asus-armoury/attributes — exiting. This almost \
                 always means the kernel's `asus-armoury` firmware-attributes driver is not \
                 loaded (too old a kernel, or booted into a rescue/LTS kernel that predates it). \
                 Check `journalctl -k | grep asus` and `ls /sys/class/firmware-attributes/`; \
                 booting the distro's regular (non-LTS) kernel usually resolves this. Under \
                 systemd this failure repeats on every restart — StartLimitBurst in hpd.service \
                 caps the loop; see `systemctl status hpd` once it trips."
            );
            return Err(e.into());
        }
    };

    // Share the backend behind an Arc so both the Executor (which owns
    // the command path) and the D-Bus interface (which reads live fan /
    // temperature telemetry on demand) can reach the same instance.
    let backend: std::sync::Arc<dyn hpd_capabilities::backend::HwBackend> =
        std::sync::Arc::new(backend);

    // systemd's StateDirectory= injects STATE_DIRECTORY (e.g. /var/lib/hpd).
    // Outside systemd we honour the config's `state_path`. /var/tmp is
    // intentionally avoided: world-writable + survives reboots = symlink-race
    // surface when running as root.
    let state_path = std::env::var("STATE_DIRECTORY")
        .map(|d| PathBuf::from(d).join("state.toml"))
        .unwrap_or_else(|_| daemon_config.state_path.clone());
    info!(path = %state_path.display(), "Using state file");
    let persister = StatePersister::new(state_path);

    // Backends without ChargeControl have no battery view of the world;
    // a missing capability is treated as "AC unknown → assume DC" so
    // the daemon falls back to the conservative power envelope.
    let is_physically_plugged = backend
        .charge()
        .and_then(|c| c.is_ac_connected().ok())
        .unwrap_or(false);

    let mut initial_state = match persister.load().await {
        Some(mut state) => {
            info!("Previous state loaded successfully from disk.");
            state.is_ac_connected = is_physically_plugged;
            state
        }
        None => {
            info!("No previous state found (or failed to read). Defaulting to hardware values...");
            // First time after installation: read the kernel's current
            // view through the capability accessors. Each fallback maps
            // to a sensible default when the backend (a) does not expose
            // the capability or (b) the read itself fails.
            let current_target = backend.power().get_target().unwrap_or(PowerEnvelopeTarget {
                spl: limits.spl_min,
                sppt: limits.spl_min,
                fppt: Some(limits.spl_min),
            });
            let current_profile = backend
                .profile()
                .and_then(|p| p.get_active_profile().ok())
                .unwrap_or(ProfileName::Balanced);
            let current_charge_limit = backend
                .charge()
                .and_then(|c| c.get_end_threshold().ok())
                .unwrap_or(daemon_config.default_charge_threshold);

            ProfileState {
                power_target: current_target,
                active_profile: current_profile,
                charge_end_threshold: current_charge_limit,
                fan_follows_tdp: true,
                is_ac_connected: is_physically_plugged,
                last_dc_state: None,
                active_fan_curve: None,
                // First boot: seed the lock preference from config (default
                // on). After this it lives in state.toml and is toggled at
                // runtime via set_ac_max_performance.
                ac_max_performance: daemon_config.default_ac_max_performance,
                ac_locked: false,
                // GPU clock auto-follow defaults OFF on every fresh
                // install — the daemon never touches
                // power_dpm_force_performance_level/pp_od_clk_voltage
                // until the user explicitly opts in once.
                active_gpu_clock: None,
                gpu_follows_tdp: false,
            }
        }
    };

    // Build the *intended* boot state, then re-assert it onto the
    // hardware so what the daemon reports always matches the device from
    // t=0. Two fields are overridden from their persisted values:
    //
    //   * `active_profile` ← the configured default (Performance). The
    //     platform profile is a decoupled power lever; pinning it on every
    //     boot keeps the SPL fully usable and migrates a device left in a
    //     throttling profile by an older hpd.
    //   * `active_fan_curve` ← the persisted selection, else the config's
    //     first-boot default. (`power_target` and `charge_end_threshold`
    //     keep their persisted values — the user's choices.)
    //
    // A cold boot resets several firmware knobs to their defaults
    // (platform_profile → balanced, charge limit → 100%, …) which the
    // daemon does NOT otherwise re-apply — so it would report a persisted
    // value the hardware no longer has. Sending `SystemResumed` below
    // re-applies envelope + profile + charge + curve **unconditionally**
    // (the same path resume uses), closing that drift in one shot.
    initial_state.active_fan_curve = initial_state.active_fan_curve.or_else(|| {
        daemon_config
            .default_fan_curve
            .map(FanCurveSelection::Preset)
    });
    initial_state.active_profile = daemon_config.default_platform_profile.clone();

    // 6. Create communication channels
    let (tx, rx) = mpsc::channel::<Transition>(daemon_config.channel_capacity);
    let internal_tx = tx.clone(); // For rollback

    // Re-assert the full intended state onto the hardware at boot (see
    // above). `SystemResumed` applies envelope → profile → charge → curve
    // in that order (curve last, since writing the profile can reset the
    // EC's custom curve). The channel buffers this until the executor
    // starts draining it.
    if tx.send(Transition::SystemResumed).await.is_err() {
        warn!("Executor channel closed before boot state could be applied");
    }

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
        backend.clone(),
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
    // synchronous property reads), one drives the PropertiesChanged
    // emitter task spawned below, and one seeds the PPD compat shim —
    // all four (including the one `PowerDaemonInterface::new` below
    // consumes) observe the same executor-published state.
    let state_rx_for_watcher = state_rx.clone();
    let state_rx_for_ppd_shim = state_rx.clone();

    // The PPD compat shim (see `hpd_dbus::ppd_shim`) is built and served
    // unconditionally — serving an object nobody can address is harmless
    // — but only *claims* its bus name after `build()`, best-effort (see
    // below): claiming it must never abort daemon startup. Whether that
    // succeeds is unknown until after `dbus_interface` already has to
    // exist, so it gets a shared flag to flip once there's an answer
    // (`get_ppd_shim_active` reads it back for `hpdctl status`/`doctor`).
    let ppd_shim = hpd_dbus::ppd_shim::PowerProfilesShim::new(tx.clone(), state_rx_for_ppd_shim);
    let ppd_shim_handle = ppd_shim.handle();
    let ppd_shim_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let dbus_interface = hpd_dbus::service::PowerDaemonInterface::new(
        tx.clone(),
        state_rx,
        limits,
        backend,
        std::sync::Arc::clone(&ppd_shim_active),
    );

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
        .serve_at(hpd_dbus::ppd_shim::OBJECT_PATH, ppd_shim)?
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

    // Try to claim the PPD compat name. Deliberately `DoNotQueue` with no
    // `ReplaceExisting`/`AllowReplacement`: if the real `power-profiles-daemon`
    // or `tuned-ppd` is live (i.e. not masked — see `hpd_dbus::conflicts`),
    // hpd must back off rather than steal the name out from under it. Only
    // once the name is actually granted do the shim's background tasks
    // (external-override reconciliation, holder-disconnect detection) get
    // spawned — an unclaimed shim would otherwise leak a `NameOwnerChanged`
    // subscription for no reason.
    match conn
        .request_name_with_flags(
            hpd_dbus::ppd_shim::BUS_NAME,
            zbus::fdo::RequestNameFlags::DoNotQueue.into(),
        )
        .await
    {
        Ok(zbus::fdo::RequestNameReply::PrimaryOwner) => {
            ppd_shim_active.store(true, std::sync::atomic::Ordering::Relaxed);
            info!(
                name = hpd_dbus::ppd_shim::BUS_NAME,
                "PPD compat shim active — the KDE power applet, `powerprofilesctl`, and \
                 CachyOS's `game-performance` now control hpd"
            );
            let ppd_iface_ref = conn
                .object_server()
                .interface::<_, hpd_dbus::ppd_shim::PowerProfilesShim>(
                    hpd_dbus::ppd_shim::OBJECT_PATH,
                )
                .await?;
            tokio::spawn(hpd_dbus::ppd_shim::run_external_override_reconciler(
                ppd_shim_handle.clone(),
                ppd_iface_ref,
            ));
            tokio::spawn(hpd_dbus::ppd_shim::run_holder_disconnect_watcher(
                conn.clone(),
                ppd_shim_handle,
            ));
        }
        Ok(_) => {
            warn!(
                name = hpd_dbus::ppd_shim::BUS_NAME,
                "PPD compat shim inactive: that name is already owned (a real \
                 power-profiles-daemon or tuned-ppd is not masked). The KDE power applet and \
                 `game-performance` will not see hpd. Run `hpdctl doctor --fix`."
            );
        }
        Err(e) => {
            warn!(
                error = %e,
                name = hpd_dbus::ppd_shim::BUS_NAME,
                "PPD compat shim inactive: could not request its bus name"
            );
        }
    }

    // Self-check: confirm polkit knows our privileged actions. A partial
    // install (binary copied without package/polkit/*) leaves them
    // unregistered, so every privileged command fails with an opaque
    // AuthFailed and no obvious cause. Warn loudly and keep running —
    // telemetry/status still work, the wheel-passwordless grant comes
    // back the moment the policy lands, and the policy can be installed
    // without restarting the daemon. `hpdctl status` reads the same check
    // over D-Bus (get_diagnostics) so the user sees it without journalctl.
    match hpd_dbus::polkit::missing_actions(&conn).await {
        Ok(missing) if missing.is_empty() => {
            info!("polkit: all privileged actions are registered");
        }
        Ok(missing) => {
            warn!(
                missing = ?missing,
                count = missing.len(),
                "polkit: hpd actions are NOT registered — every privileged command (TDP / charge / \
                 profile / fan) will be denied. The polkit policy/rules are not installed. \
                 Fix it in one step: run `hpdctl fix-polkit` (prompts for admin, installs the policy, \
                 reloads polkit — no daemon restart needed). Alternatively reinstall via ./install.sh \
                 or the AUR package."
            );
        }
        Err(e) => {
            warn!(
                error = %e,
                "polkit: could not verify action registration (is polkit running?). \
                 Privileged commands may be denied."
            );
        }
    }

    // Self-check: warn if a competing power daemon is live. PPD and
    // steamos-manager write the same TDP / platform_profile / charge
    // surfaces, so co-running them makes the effective state flap and
    // hpd's settings may not stick. The daemon only detects — it is
    // sandboxed (ProtectSystem=strict) and cannot stop another service;
    // the repair is the user-side `hpdctl doctor --fix`. Also exposed live
    // over D-Bus via get_power_conflicts for `hpdctl doctor` / the plugin.
    let conflicts = hpd_dbus::conflicts::power_conflicts(&conn).await;
    if conflicts.is_empty() {
        info!(
            "power ownership: no competing power daemons detected — hpd is the sole power manager"
        );
    } else {
        warn!(
            conflicts = ?conflicts,
            "power ownership: competing power daemon(s) live ({}) — they fight hpd over TDP / \
             platform_profile / charge, so settings may not stick. Make hpd the sole manager in \
             one step: run `hpdctl doctor --fix`.",
            conflicts.join(", ")
        );
    }

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
        if new.fan_follows_tdp != last.fan_follows_tdp {
            // The D-Bus property name is `auto_cooling`; the internal
            // state field is `fan_follows_tdp`. Lote 42 introduces the
            // property so status widgets can observe the mode without
            // inferring it from observed behaviour.
            if let Err(e) = iface.auto_cooling_changed(ctx).await {
                error!(error = %e, "Failed to emit auto_cooling PropertiesChanged");
            }
        }
        if new.active_fan_curve != last.active_fan_curve {
            if let Err(e) = iface.fan_curve_changed(ctx).await {
                error!(error = %e, "Failed to emit fan_curve PropertiesChanged");
            }
        }
        if new.active_gpu_clock != last.active_gpu_clock {
            if let Err(e) = iface.gpu_clock_range_changed(ctx).await {
                error!(error = %e, "Failed to emit gpu_clock_range PropertiesChanged");
            }
        }
        if new.gpu_follows_tdp != last.gpu_follows_tdp {
            if let Err(e) = iface.gpu_follows_tdp_changed(ctx).await {
                error!(error = %e, "Failed to emit gpu_follows_tdp PropertiesChanged");
            }
        }
        if new.is_ac_connected != last.is_ac_connected {
            if let Err(e) = iface.ac_connected_changed(ctx).await {
                error!(error = %e, "Failed to emit ac_connected PropertiesChanged");
            }
        }
        if new.ac_locked != last.ac_locked {
            // Drives the plugin's "locked to max performance on AC" state so
            // it can disable/re-enable the power + cooling controls live.
            if let Err(e) = iface.ac_locked_changed(ctx).await {
                error!(error = %e, "Failed to emit ac_locked PropertiesChanged");
            }
        }
        if new.ac_max_performance != last.ac_max_performance {
            // The lock *preference* toggled — drives the plugin's toggle state.
            if let Err(e) = iface.ac_max_performance_changed(ctx).await {
                error!(error = %e, "Failed to emit ac_max_performance PropertiesChanged");
            }
        }

        last = new;
    }
}
