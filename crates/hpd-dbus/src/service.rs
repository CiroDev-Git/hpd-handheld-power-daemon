use zbus::interface;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error};

use hpd_core::transition::Transition;
use hpd_core::state::ProfileState;
use hpd_capabilities::units::PowerMilliwatts;

pub struct PowerDaemonInterface {
    tx: mpsc::Sender<Transition>,
    state_rx: watch::Receiver<ProfileState>,
}

impl PowerDaemonInterface {
    pub fn new(tx: mpsc::Sender<Transition>, state_rx: watch::Receiver<ProfileState>) -> Self {
        Self { tx, state_rx }
    }
}

#[interface(name = "dev.cirodev.hpd.PowerDaemon1")]
impl PowerDaemonInterface {
    
    /// Change SPL
    async fn set_spl(&self, watts: u32) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set SPL: {}W", watts);
        
        // 1. Get current target from memory (avoid blocking)
        let mut target = self.state_rx.borrow().power_target.clone();
        
        // 2. Modify only SPL (convertion from W to mW for L3 domain)
        target.spl = PowerMilliwatts(watts * 1000);
        
        // 3. Push event to Executor
        if self.tx.send(Transition::SetEnvelope(target)).await.is_err() {
            error!("Failed to send transition to executor");
            return Err(zbus::fdo::Error::Failed("Internal daemon error: Executor down".into()));
        }
        
        Ok(())
    }

    #[zbus(property)]
    async fn current_spl(&self) -> u32 {
        let spl_mw = self.state_rx.borrow().power_target.spl.0;
        spl_mw / 1000 // Convertion from mW to W for UI
    }
}