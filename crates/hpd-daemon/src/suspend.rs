// SPDX-License-Identifier: GPL-3.0-or-later

use futures_util::stream::StreamExt;
use hpd_core::transition::Transition;
use tokio::sync::mpsc;
use tracing::{error, info, warn};
use zbus::Connection;

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait LoginManager {
    /// logind signal:
    /// start = true (to sleep) | start = false (wake up)
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

/// Backoff before re-subscribing to logind after the signal stream drops or
/// a connection attempt fails. Short, but non-zero so a hard failure can't
/// busy-loop.
const RECONNECT_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// Why a single logind monitor session ended.
enum SessionEnd {
    /// The executor channel closed (daemon shutting down) — stop for good.
    ExecutorGone,
    /// The signal stream ended (the bus connection dropped) — reconnect.
    StreamLost,
    /// Could not connect / build the proxy / subscribe — log + retry.
    SetupFailed,
}

pub async fn spawn_suspend_monitor(tx: mpsc::Sender<Transition>) {
    info!("Starting sleep monitor (systemd-logind)...");

    // Outer reconnect loop. The logind `PrepareForSleep` signal stream can end
    // if the system-bus connection drops (e.g. perturbed by a suspend). The
    // old `while let Some(...)` would then exit and stop detecting resume for
    // the rest of the process (GAP #1 of the 2026-06 lifecycle audit) — the
    // daemon would never re-assert state on later wakes. Re-subscribe instead;
    // only a dropped `tx` (executor gone) stops the monitor for good.
    loop {
        match run_monitor_session(&tx).await {
            SessionEnd::ExecutorGone => {
                info!("Suspend monitor: executor unavailable. Stopping.");
                return;
            }
            SessionEnd::StreamLost => warn!("logind sleep-signal stream ended; reconnecting..."),
            SessionEnd::SetupFailed => { /* already logged below */ }
        }
        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

/// One logind connection + subscription. Forwards each resume edge as a
/// [`Transition::SystemResumed`]. Returns the reason it ended.
async fn run_monitor_session(tx: &mpsc::Sender<Transition>) -> SessionEnd {
    let conn = match Connection::system().await {
        Ok(c) => c,
        Err(e) => {
            error!("Unable to connect to System Bus for logind: {}", e);
            return SessionEnd::SetupFailed;
        }
    };

    let proxy = match LoginManagerProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            error!("Unable to create logind proxy: {}", e);
            return SessionEnd::SetupFailed;
        }
    };

    let mut stream = match proxy.receive_prepare_for_sleep().await {
        Ok(s) => s,
        Err(e) => {
            error!("Failed to subscribe to sleep signal: {}", e);
            return SessionEnd::SetupFailed;
        }
    };

    // Sleep loop (0% CPU). Awake only with Sleep/Resume events.
    while let Some(signal) = stream.next().await {
        match signal.args() {
            Ok(args) => {
                if args.start {
                    info!("💤 Preparing system to sleep...");
                } else {
                    info!("🌅 Resume system. Starting again...");
                    if tx.send(Transition::SystemResumed).await.is_err() {
                        return SessionEnd::ExecutorGone;
                    }
                }
            }
            Err(e) => error!("Failed to decode logind signal: {}", e),
        }
    }
    SessionEnd::StreamLost
}
