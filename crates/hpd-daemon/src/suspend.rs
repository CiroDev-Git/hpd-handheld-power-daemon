use hpd_core::transition::Transition;
use tokio::sync::mpsc;
use tracing::{error, info};
use zbus::Connection;
use futures_util::stream::StreamExt;

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

pub async fn spawn_suspend_monitor(tx: mpsc::Sender<Transition>) {
    info!("Starting sleep monitor (systemd-logind)...");

    let conn = match Connection::system().await {
        Ok(c) => c,
        Err(e) => {
            error!("Unable to connect to System Bus for logind: {}", e);
            return;
        }
    };

    let proxy = match LoginManagerProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            error!("Unable to create logind proxy: {}", e);
            return;
        }
    };

    let mut stream = match proxy.receive_prepare_for_sleep().await {
        Ok(s) => s,
        Err(e) => {
            error!("Failure in subscription to sleep signal: {}", e);
            return;
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
                    let _ = tx.send(Transition::SystemResumed).await;
                }
            }
            Err(e) => error!("Failure decode in logind signal: {}", e),
        }
    }
}