use hpd_core::transition::Transition;
use tokio::sync::mpsc;
use tracing::{error, info, debug};

// ========================================================
// PRODUCTION MODE (Compily on Linux)
// ========================================================
#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use futures_util::StreamExt;
    use tokio_udev::{AsyncMonitorSocket, MonitorBuilder};

    const SUBSYS_POWER: &str = "power_supply";
    const PROP_ONLINE: &str = "POWER_SUPPLY_ONLINE";
    const VAL_ONLINE_TRUE: &str = "1";
    const IDENTIFIER_AC: &str = "AC";
    const IDENTIFIER_ADP: &str = "ADP";

    pub async fn spawn_power_monitor(tx: mpsc::Sender<Transition>) {
        info!("Starting Netlink monitor (udev) for energy events...");

        // 1. Only detect changes in "power_supply"
        let builder = match MonitorBuilder::new() {
            Ok(b) => b,
            Err(e) => {
                error!("Failure creating udev monitor: {}", e);
                return;
            }
        };

        let builder = match builder.match_subsystem(SUBSYS_POWER) {
            Ok(b) => b,
            Err(e) => {
                error!("Failure getting power_supply subsystem: {}", e);
                return;
            }
        };

        // 2. Open Netlink socket
        let monitor = match builder.listen() {
            Ok(m) => m,
            Err(e) => {
                error!("Failure with socket udev detection: {}", e);
                return;
            }
        };

        let mut async_monitor = match AsyncMonitorSocket::new(monitor) {
            Ok(m) => m,
            Err(e) => {
                error!("Failure convertion with async monitor: {}", e);
                return;
            }
        };

        debug!("Netlink monitor ready. Awaiting for AC/DC events...");

        // 3. Infinite sleeping loop (0% CPU). Only awake when event happen
        while let Some(Ok(event)) = async_monitor.next().await {
            let sysname = event.sysname();
            let name = sysname.to_string_lossy().to_uppercase();
            if name.contains(IDENTIFIER_AC) || name.contains(IDENTIFIER_ADP) {
                            
                // ONLINE (1 = Connected, 0 = Disconnected)
                if let Some(online_val) = event.property_value(PROP_ONLINE) {
                    let is_ac_plugged: bool = online_val == VAL_ONLINE_TRUE;
                    
                    info!("⚡ Hardware event detected: Charger connected = {}", is_ac_plugged);

                    if tx.send(Transition::AcPowerChanged(is_ac_plugged)).await.is_err() {
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