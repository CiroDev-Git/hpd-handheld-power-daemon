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
    use tracing::{debug, error, warn};

    const SUBSYS_POWER: &str = "power_supply";

    /// Backoff before re-establishing the udev monitor after the stream ends
    /// or a build fails. Short (AC edges should be picked up promptly) but
    /// non-zero so a hard failure can't busy-loop.
    const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

    /// Build a fresh `power_supply` async monitor socket.
    fn build_monitor() -> Result<AsyncMonitorSocket, String> {
        let builder = MonitorBuilder::new().map_err(|e| format!("create monitor: {e}"))?;
        let builder = builder
            .match_subsystem(SUBSYS_POWER)
            .map_err(|e| format!("filter power_supply: {e}"))?;
        let monitor = builder
            .listen()
            .map_err(|e| format!("open udev socket: {e}"))?;
        AsyncMonitorSocket::new(monitor).map_err(|e| format!("async monitor: {e}"))
    }

    /// Re-read the canonical mains node; if it changed vs `last`, forward the
    /// edge to `tx`. `Err(())` means the executor channel is closed (daemon
    /// shutting down) — the signal to stop the monitor for good.
    async fn emit_if_changed(
        tx: &mpsc::Sender<Transition>,
        root: &Path,
        last: &mut Option<bool>,
    ) -> Result<(), ()> {
        let current = read_mains_online_at(root);
        if let Some(is_ac_plugged) = current {
            if *last != current {
                *last = current;
                info!(
                    "⚡ Hardware event detected: Charger connected = {}",
                    is_ac_plugged
                );
                if tx
                    .send(Transition::AcPowerChanged(is_ac_plugged))
                    .await
                    .is_err()
                {
                    error!("Netlink monitor: executor unavailable. Stopping monitor.");
                    return Err(());
                }
            }
        }
        Ok(())
    }

    /// Run the udev `power_supply` monitor, forwarding every AC-plug /
    /// AC-unplug edge to `tx` as a [`Transition::AcPowerChanged`].
    ///
    /// Wrapped in an **outer reconnect loop**: an `AsyncMonitorSocket` stream
    /// can end (`None`) or yield an `Err` — e.g. when a suspend perturbs the
    /// netlink socket. The old `while let Some(Ok(_))` fell straight out on
    /// the first such item and silently killed the monitor for the rest of
    /// the process, stopping live AC detection until the daemon restarted
    /// (GAP #1 of the 2026-06 lifecycle audit). We now log, back off, rebuild
    /// the socket, and on every (re)connect reconcile the canonical mains node
    /// so an edge that happened while we were down (e.g. unplugged mid-suspend)
    /// is still emitted. Only a dropped `tx` (executor gone, daemon shutting
    /// down) stops the monitor for good.
    pub async fn spawn_power_monitor(tx: mpsc::Sender<Transition>) {
        info!("Starting Netlink monitor (udev) for energy events...");
        let root = Path::new(POWER_SUPPLY_ROOT);
        // Seed last-known state from sysfs so the monitor's own startup is not
        // mistaken for an edge — only genuine plug/unplug edges fire.
        let mut last_state = read_mains_online_at(root);

        loop {
            match build_monitor() {
                Ok(mut async_monitor) => {
                    debug!("Netlink monitor ready. Awaiting AC/DC events...");
                    // Reconcile on (re)connect: emit any edge missed while down.
                    if emit_if_changed(&tx, root, &mut last_state).await.is_err() {
                        return;
                    }
                    // We do NOT trust the event's own node/ONLINE: on USB-C
                    // handhelds the edge fires on the PD port, not the mains
                    // node, so on *any* power_supply event we re-read the
                    // canonical mains node and forward only deduped edges.
                    loop {
                        match async_monitor.next().await {
                            Some(Ok(_event)) => {
                                if emit_if_changed(&tx, root, &mut last_state).await.is_err() {
                                    return;
                                }
                            }
                            Some(Err(e)) => {
                                warn!("udev monitor error: {}; rebuilding socket...", e);
                                break;
                            }
                            None => {
                                warn!("udev monitor stream ended; rebuilding socket...");
                                break;
                            }
                        }
                    }
                }
                Err(e) => error!("Failed to build udev monitor: {}; retrying...", e),
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
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
