use zbus::interface;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error};

use hpd_core::transition::Transition;
use hpd_core::state::ProfileState;
use hpd_capabilities::units::PowerMilliwatts;
use hpd_core::invariants::validate_power_envelope;

use hpd_capabilities::charge::{MIN_CHARGE_THRESHOLD, MAX_CHARGE_THRESHOLD};

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
        let spl_mw = watts * 1000;
        target.spl = PowerMilliwatts(spl_mw);

        // 3. Boost curve
        // SPPT = SPL + 15% 
        // FPPT = SPL + 25%
        let sppt_mw = (spl_mw as f32 * 1.15) as u32; 
        let fppt_mw = (spl_mw as f32 * 1.25) as u32;
        target.sppt = PowerMilliwatts(sppt_mw);
        target.fppt = Some(PowerMilliwatts(fppt_mw));

        // 4. Check before queoe.
        if let Err(e) = validate_power_envelope(&target) {
            error!("D-Bus rejected request: {}", e);
            return Err(zbus::fdo::Error::InvalidArgs(e.to_string()));
        }
        
        // 5. Push event to Executor
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

    async fn set_charge_threshold(&self, threshold: u8) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set Charge Limit: {}%", threshold);
        
        if !(MIN_CHARGE_THRESHOLD..=MAX_CHARGE_THRESHOLD).contains(&threshold) {
            return Err(zbus::fdo::Error::InvalidArgs("Charge limit must be between 20 and 100".into()));
        }

        if self.tx.send(Transition::ChargeThresholdChanged(threshold)).await.is_err() {
            error!("Failed to send transition to executor");
            return Err(zbus::fdo::Error::Failed("Internal daemon error: Executor down".into()));
        }
        
        Ok(())
    }

    #[zbus(property)]
    async fn active_profile(&self) -> String {
        let profile = &self.state_rx.borrow().active_profile;
        format!("{:?}", profile) 
    }

    #[zbus(property)]
    async fn charge_end_threshold(&self) -> u8 {
        self.state_rx.borrow().charge_end_threshold
    }
}