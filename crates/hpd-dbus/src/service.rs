use zbus::interface;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error};
use std::str::FromStr;

use hpd_core::transition::Transition;
use hpd_core::state::ProfileState;
use hpd_capabilities::power::PowerEnvelopeLimits;
use hpd_capabilities::profile::ProfileName;

use hpd_capabilities::charge::{MIN_CHARGE_THRESHOLD, MAX_CHARGE_THRESHOLD};

pub struct PowerDaemonInterface {
    tx: mpsc::Sender<Transition>,
    state_rx: watch::Receiver<ProfileState>,
    limits: PowerEnvelopeLimits,
}

impl PowerDaemonInterface {
    pub fn new(
        tx: mpsc::Sender<Transition>, 
        state_rx: watch::Receiver<ProfileState>,
        limits: PowerEnvelopeLimits,
    ) -> Self {
        Self { tx, state_rx, limits }
    }
}

#[interface(name = "dev.cirodev.hpd.PowerDaemon1")]
impl PowerDaemonInterface {
    
    /// Change SPL
   async fn set_spl(&self, watts: u32) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set SPL: {}W", watts);
        
        if self.tx.send(Transition::SetSpl(watts)).await.is_err() {
            error!("Failed to send transition to executor");
            return Err(zbus::fdo::Error::Failed("Internal daemon error: Executor down".into()));
        }
        
        Ok(())
    }

    async fn set_preset(&self, preset_name: String) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set Preset: {}", preset_name);
        
        let preset = preset_name.parse::<hpd_capabilities::profile::SystemPreset>()
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e))?;

        if self.tx.send(Transition::SetPreset(preset)).await.is_err() {
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

    async fn get_hardware_limits(&self) -> zbus::fdo::Result<(u32, u32, u32, u32)> {
        Ok((
            self.limits.spl_min.0 / 1000,
            self.limits.spl_max.0 / 1000,
            self.limits.sppt_max.0 / 1000,
            self.limits.fppt_max.0 / 1000,
        ))
    }

    async fn is_ac_connected(&self) -> zbus::fdo::Result<bool> {
        Ok(self.state_rx.borrow().is_ac_connected)
    }

    async fn set_profile(&self, profile: &str) -> zbus::fdo::Result<()> {
        let profile_enum = ProfileName::from_str(profile)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e))?;

        let _ = self.tx.send(Transition::SetProfile(profile_enum)).await;
        Ok(())
    }

    async fn set_fan_auto(&self) -> zbus::fdo::Result<()> {
        let _ = self.tx.send(Transition::EnableFanAuto).await;
        Ok(())
    }
}