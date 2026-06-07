// SPDX-License-Identifier: GPL-3.0-or-later

//! AC/DC power-source monitor (workspace layer **L0**).
//!
//! Subscribes to udev `power_supply` events on Linux and emits
//! [`Transition::AcPowerChanged`] every time the charger is plugged or
//! unplugged. On non-Linux targets the crate compiles down to a no-op
//! that simply awaits `pending()` forever — this keeps the daemon
//! buildable on macOS/dev hosts where there is no netlink socket.
//!
//! The Linux path runs `tokio-udev`'s `AsyncMonitorSocket`, which is
//! `!Send`; the daemon hosts it on a dedicated `std::thread` with its
//! own current-thread runtime + `LocalSet`. See
//! [`hpd-daemon`'s `main.rs`](../hpd_daemon/index.html) for the wiring.

use hpd_core::transition::Transition;
use std::path::Path;
use tokio::sync::mpsc;
use tracing::info;

/// The kernel power-supply class root. The canonical mains-adapter state
/// lives under the `type == "Mains"` node here (e.g. `AC0`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const POWER_SUPPLY_ROOT: &str = "/sys/class/power_supply";

/// Reads the real AC-adapter state from the kernel `power_supply` class
/// under `base`: `Some(true)` if **any** `type == "Mains"` supply reports
/// `online == 1`, `Some(false)` if mains nodes exist but none are online,
/// `None` if no readable mains node is present.
///
/// We re-read the canonical mains node instead of trusting a udev event's
/// own `POWER_SUPPLY_ONLINE`, because on USB-C-charged handhelds (ROG Ally
/// X) the plug/unplug **event** fires on the USB-C PD port
/// (`ucsi-source-psy-USBC000:*`, `type == "USB"`) — *not* on the mains
/// node — even though the kernel updates `AC0/online` correctly. Filtering
/// events by an `AC`/`ADP` sysname therefore misses every USB-C edge.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn read_mains_online_at(base: &Path) -> Option<bool> {
    let mut found_mains = false;
    let mut any_online = false;
    for entry in std::fs::read_dir(base).ok()?.flatten() {
        let node = entry.path();
        let kind = std::fs::read_to_string(node.join("type")).unwrap_or_default();
        if kind.trim() == "Mains" {
            found_mains = true;
            let online = std::fs::read_to_string(node.join("online")).unwrap_or_default();
            if online.trim() == "1" {
                any_online = true;
            }
        }
    }
    found_mains.then_some(any_online)
}

// ========================================================
// PRODUCTION MODE (Compily on Linux)
// ========================================================
#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use futures_util::StreamExt;
    use tokio_udev::{AsyncMonitorSocket, MonitorBuilder};
    use tracing::{debug, error};

    const SUBSYS_POWER: &str = "power_supply";

    /// Block the current task on the udev `power_supply` subsystem and
    /// forward every AC-plug / AC-unplug edge to `tx` as a
    /// [`Transition::AcPowerChanged`].
    ///
    /// Returns when either (a) the udev socket errors out terminally or
    /// (b) the executor on the other end of `tx` is dropped (daemon
    /// shutting down). All other errors are logged and the loop keeps
    /// running — AC events are recoverable.
    pub async fn spawn_power_monitor(tx: mpsc::Sender<Transition>) {
        info!("Starting Netlink monitor (udev) for energy events...");

        // 1. Only detect changes in "power_supply"
        let builder = match MonitorBuilder::new() {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to create udev monitor: {}", e);
                return;
            }
        };

        let builder = match builder.match_subsystem(SUBSYS_POWER) {
            Ok(b) => b,
            Err(e) => {
                error!("Failed to filter power_supply subsystem: {}", e);
                return;
            }
        };

        // 2. Open Netlink socket
        let monitor = match builder.listen() {
            Ok(m) => m,
            Err(e) => {
                error!("Failed to open udev socket: {}", e);
                return;
            }
        };

        let mut async_monitor = match AsyncMonitorSocket::new(monitor) {
            Ok(m) => m,
            Err(e) => {
                error!("Failed to convert udev monitor into async monitor: {}", e);
                return;
            }
        };

        debug!("Netlink monitor ready. Awaiting for AC/DC events...");

        let root = Path::new(POWER_SUPPLY_ROOT);

        // Seed the last-known state from sysfs so the first real edge — not
        // the monitor's own startup — is what produces a transition.
        let mut last_state = read_mains_online_at(root);

        // 3. Infinite sleeping loop (0% CPU). Only awake when an event
        //    happens. We do NOT trust the event's own node/ONLINE: on USB-C
        //    handhelds the edge fires on the PD port, not the mains node, so
        //    on *any* power_supply event we re-read the canonical mains node
        //    and forward only genuine plug/unplug edges (deduped).
        while let Some(Ok(_event)) = async_monitor.next().await {
            let current = read_mains_online_at(root);
            if let Some(is_ac_plugged) = current {
                if last_state != current {
                    last_state = current;
                    info!(
                        "⚡ Hardware event detected: Charger connected = {}",
                        is_ac_plugged
                    );

                    if tx
                        .send(Transition::AcPowerChanged(is_ac_plugged))
                        .await
                        .is_err()
                    {
                        error!("Netlink monitor: Main executor not available. Stopping monitor.");
                        break;
                    }
                }
            }
        }
    }
}

// ========================================================
// SIMULATOR MODE (Compile on macOS)
// ========================================================
#[cfg(not(target_os = "linux"))]
mod dummy {
    use super::*;
    /// No-op stand-in for `spawn_power_monitor` on non-Linux targets.
    /// Awaits a never-resolving future so the daemon's `select!` loop
    /// keeps the binary alive without consuming CPU.
    pub async fn spawn_power_monitor(_tx: mpsc::Sender<Transition>) {
        info!("AC Monitor disabled on macOS (Simulator mode).");
        // Sleeping thread without CPU consume
        std::future::pending::<()>().await;
    }
}

#[cfg(target_os = "linux")]
pub use linux::spawn_power_monitor;

#[cfg(not(target_os = "linux"))]
pub use dummy::spawn_power_monitor;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// Build a fake `/sys/class/power_supply` tree under a unique temp dir.
    fn fixture(nodes: &[(&str, &str, Option<&str>)]) -> std::path::PathBuf {
        // Atomic counter guarantees a unique dir even for parallel tests
        // landing on the same nanosecond.
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("hpd-netlink-test-{}-{}", std::process::id(), n));
        std::fs::remove_dir_all(&base).ok(); // clear any stale leftover
        for (name, kind, online) in nodes {
            let node = base.join(name);
            std::fs::create_dir_all(&node).unwrap();
            std::fs::write(node.join("type"), format!("{kind}\n")).unwrap();
            if let Some(o) = online {
                std::fs::write(node.join("online"), format!("{o}\n")).unwrap();
            }
        }
        base
    }

    #[test]
    fn mains_online_reflects_ac0_not_the_usb_c_port() {
        // ROG Ally X shape: AC0 is the Mains node; the USB-C PD ports are
        // type=USB. Charging via USB-C sets AC0/online=1.
        let base = fixture(&[
            ("AC0", "Mains", Some("1")),
            ("BAT0", "Battery", None),
            ("ucsi-source-psy-USBC000:002", "USB", Some("1")),
        ]);
        assert_eq!(read_mains_online_at(&base), Some(true));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn mains_offline_when_ac0_online_zero_ignoring_usb() {
        // A USB device reporting online=1 must NOT count as AC.
        let base = fixture(&[
            ("AC0", "Mains", Some("0")),
            ("ucsi-source-psy-USBC000:001", "USB", Some("1")),
        ]);
        assert_eq!(read_mains_online_at(&base), Some(false));
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn no_mains_node_is_none() {
        let base = fixture(&[("BAT0", "Battery", None)]);
        assert_eq!(read_mains_online_at(&base), None);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn missing_root_is_none() {
        let base = std::env::temp_dir().join("hpd-netlink-does-not-exist-xyz");
        assert_eq!(read_mains_online_at(&base), None);
    }
}
