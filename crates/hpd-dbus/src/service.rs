use zbus::interface;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error};
use std::str::FromStr;

use hpd_core::transition::Transition;
use hpd_core::state::ProfileState;
use hpd_capabilities::power::PowerEnvelopeLimits;
use hpd_capabilities::profile::ProfileName;

use hpd_capabilities::charge::{MIN_CHARGE_THRESHOLD, MAX_CHARGE_THRESHOLD};

use crate::actions::PolkitAction;
use crate::polkit;

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

fn auth_denied() -> zbus::fdo::Error {
    zbus::fdo::Error::AuthFailed("Not authorized by polkit".into())
}

fn executor_down() -> zbus::fdo::Error {
    zbus::fdo::Error::Failed("Internal daemon error: Executor down".into())
}

#[interface(name = "dev.cirodev.hpd.PowerDaemon1")]
impl PowerDaemonInterface {

    /// Change SPL
    async fn set_spl(
        &self,
        watts: u32,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set SPL: {}W", watts);
        if !polkit::check(conn, &header, PolkitAction::SetTdp).await {
            return Err(auth_denied());
        }
        if self.tx.send(Transition::SetSpl(watts)).await.is_err() {
            error!("Failed to send transition to executor");
            return Err(executor_down());
        }
        Ok(())
    }

    async fn set_preset(
        &self,
        preset_name: String,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set Preset: {}", preset_name);
        if !polkit::check(conn, &header, PolkitAction::SetTdp).await {
            return Err(auth_denied());
        }
        let preset = preset_name.parse::<hpd_capabilities::profile::TdpPreset>()
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        if self.tx.send(Transition::SetPreset(preset)).await.is_err() {
            error!("Failed to send transition to executor");
            return Err(executor_down());
        }
        Ok(())
    }

    #[zbus(property)]
    async fn current_spl(&self) -> u32 {
        // UI shows whole watts; conversion lives on the value type.
        self.state_rx.borrow().power_target.spl.as_watts()
    }

    async fn set_charge_threshold(
        &self,
        threshold: u8,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set Charge Limit: {}%", threshold);
        if !(MIN_CHARGE_THRESHOLD..=MAX_CHARGE_THRESHOLD).contains(&threshold) {
            return Err(zbus::fdo::Error::InvalidArgs("Charge limit must be between 20 and 100".into()));
        }
        if !polkit::check(conn, &header, PolkitAction::SetCharge).await {
            return Err(auth_denied());
        }
        if self.tx.send(Transition::ChargeThresholdChanged(threshold)).await.is_err() {
            error!("Failed to send transition to executor");
            return Err(executor_down());
        }
        Ok(())
    }

    #[zbus(property)]
    async fn active_profile(&self) -> String {
        // ProfileName::Display is the stable D-Bus contract (kebab-case,
        // roundtrips through FromStr). Do not use Debug here — Debug is an
        // internal representation that can change with refactors.
        self.state_rx.borrow().active_profile.to_string()
    }

    #[zbus(property)]
    async fn charge_end_threshold(&self) -> u8 {
        self.state_rx.borrow().charge_end_threshold
    }

    async fn get_hardware_limits(&self) -> zbus::fdo::Result<(u32, u32, u32, u32)> {
        Ok((
            self.limits.spl_min.as_watts(),
            self.limits.spl_max.as_watts(),
            self.limits.sppt_max.as_watts(),
            self.limits.fppt_max.as_watts(),
        ))
    }

    async fn is_ac_connected(&self) -> zbus::fdo::Result<bool> {
        Ok(self.state_rx.borrow().is_ac_connected)
    }

    async fn set_profile(
        &self,
        profile: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        let profile_enum = ProfileName::from_str(profile)
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        if self.tx.send(Transition::SetProfile(profile_enum)).await.is_err() {
            return Err(executor_down());
        }
        Ok(())
    }

    async fn set_fan_auto(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        if self.tx.send(Transition::EnableFanAuto).await.is_err() {
            return Err(executor_down());
        }
        Ok(())
    }
}
