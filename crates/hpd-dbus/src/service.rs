use zbus::interface;
use tokio::sync::{mpsc, watch};
use hpd_core::transition::Transition;
use hpd_core::state::ProfileState;
use hpd_capabilities::power::PowerEnvelopeTarget;
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
    
    async fn set_spl(&self, watts: u32) -> zbus::fdo::Result<()> {
        let mut target = self.state_rx.borrow().power_target.clone();
        
        target.spl = PowerMilliwatts(watts * 1000);
        
        if self.tx.send(Transition::SetEnvelope(target)).await.is_err() {
            return Err(zbus::fdo::Error::Failed("Daemon executor is down".into()));
        }
        
        Ok(())
    }

    #[zbus(property)]
    fn current_spl(&self) -> u32 {
        self.state_rx.borrow().power_target.spl.0 / 1000
    }
    
}