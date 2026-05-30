// SPDX-License-Identifier: GPL-3.0-or-later

// zbus's `#[interface]` macro synthesises items (interface `name()`
// shim, `*_changed` signal emitters for properties) whose docs we
// can't attach via `///`. Suppress the lint module-wide; every
// human-written method in here is documented individually.
#![allow(missing_docs)]

use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error};
use zbus::interface;

use hpd_capabilities::backend::HwBackend;
use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
use hpd_capabilities::power::PowerEnvelopeLimits;
use hpd_capabilities::profile::ProfileName;
use hpd_core::state::ProfileState;
use hpd_core::transition::Transition;

use hpd_capabilities::charge::{MAX_CHARGE_THRESHOLD, MIN_CHARGE_THRESHOLD};

use crate::actions::PolkitAction;
use crate::polkit;

/// Backing object for the `dev.cirodev.hpd.PowerDaemon1` D-Bus interface.
///
/// Holds the channels that connect the D-Bus layer to the rest of the
/// daemon — an [`mpsc::Sender`] for emitting [`Transition`]s into the
/// executor, a [`watch::Receiver`] for reading the current
/// [`ProfileState`] in property getters without locking, and a frozen
/// snapshot of the hardware [`PowerEnvelopeLimits`] for the
/// `get_hardware_limits` reply.
pub struct PowerDaemonInterface {
    tx: mpsc::Sender<Transition>,
    state_rx: watch::Receiver<ProfileState>,
    limits: PowerEnvelopeLimits,
    /// Shared backend handle, used only to read live telemetry (fan RPM,
    /// temperatures) on demand. Command mutations still flow through
    /// `tx` into the executor — this handle is never used to write.
    backend: Arc<dyn HwBackend>,
}

impl PowerDaemonInterface {
    /// Build the interface from the daemon's wiring. `tx` is the
    /// command lane into the [`Executor`](hpd_core::executor::Executor);
    /// `state_rx` is the live state mirror property getters read from;
    /// `limits` is the immutable hardware envelope detected at startup;
    /// `backend` is the shared handle used for live telemetry reads.
    pub fn new(
        tx: mpsc::Sender<Transition>,
        state_rx: watch::Receiver<ProfileState>,
        limits: PowerEnvelopeLimits,
        backend: Arc<dyn HwBackend>,
    ) -> Self {
        Self {
            tx,
            state_rx,
            limits,
            backend,
        }
    }
}

/// Sentinel returned in [`PowerDaemonInterface::get_thermal_status`] for
/// a sensor or fan the hardware does not expose (or that failed to
/// read). Distinct from `0`, which is a valid reading (a stopped fan).
const TELEMETRY_UNAVAILABLE: i32 = i32::MIN;

fn auth_denied() -> zbus::fdo::Error {
    zbus::fdo::Error::AuthFailed("Not authorized by polkit".into())
}

fn executor_down() -> zbus::fdo::Error {
    zbus::fdo::Error::Failed("Internal daemon error: Executor down".into())
}

#[interface(name = "dev.cirodev.hpd.PowerDaemon1")]
impl PowerDaemonInterface {
    /// Set the Sustained Power Limit (SPL) in watts.
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-tdp` (`auth_admin`).
    /// SPPT and FPPT are derived from SPL via the runtime multipliers
    /// (`sppt_factor` / `fppt_factor`) in the executor.
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

    /// Apply a named TDP preset (`eco`, `balanced`, `max`).
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-tdp`.
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
        let preset = preset_name
            .parse::<hpd_capabilities::profile::TdpPreset>()
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        if self.tx.send(Transition::SetPreset(preset)).await.is_err() {
            error!("Failed to send transition to executor");
            return Err(executor_down());
        }
        Ok(())
    }

    /// Current SPL in whole watts (the UI never sees milliwatts).
    #[zbus(property)]
    async fn current_spl(&self) -> u32 {
        // UI shows whole watts; conversion lives on the value type.
        self.state_rx.borrow().power_target.spl.as_watts()
    }

    /// Set the battery charge end threshold (percentage, 20-100).
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-charge`.
    async fn set_charge_threshold(
        &self,
        threshold: u8,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set Charge Limit: {}%", threshold);
        if !(MIN_CHARGE_THRESHOLD..=MAX_CHARGE_THRESHOLD).contains(&threshold) {
            return Err(zbus::fdo::Error::InvalidArgs(
                "Charge limit must be between 20 and 100".into(),
            ));
        }
        if !polkit::check(conn, &header, PolkitAction::SetCharge).await {
            return Err(auth_denied());
        }
        if self
            .tx
            .send(Transition::ChargeThresholdChanged(threshold))
            .await
            .is_err()
        {
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

    /// Current battery charge end threshold (percentage).
    #[zbus(property)]
    async fn charge_end_threshold(&self) -> u8 {
        self.state_rx.borrow().charge_end_threshold
    }

    /// Whether the daemon is currently inferring the cooling profile
    /// from the TDP envelope (auto mode) or honouring the operator's
    /// last explicit `set_profile` (manual mode).
    ///
    /// Backed by `ProfileState::fan_follows_tdp`. Exposed on D-Bus so
    /// status widgets (KDE / GNOME power applets, overlay HUDs) can
    /// surface the mode without inferring it from observed behaviour.
    /// Re-enabling auto mode is `set_fan_auto`; setting any profile
    /// manually with `set_profile` flips this back to `false`.
    #[zbus(property)]
    async fn auto_cooling(&self) -> bool {
        self.state_rx.borrow().fan_follows_tdp
    }

    /// Hardware-imposed envelope limits (all watts):
    /// `(spl_min, spl_max, sppt_max, fppt_max)`.
    async fn get_hardware_limits(&self) -> zbus::fdo::Result<(u32, u32, u32, u32)> {
        Ok((
            self.limits.spl_min.as_watts(),
            self.limits.spl_max.as_watts(),
            self.limits.sppt_max.as_watts(),
            self.limits.fppt_max.as_watts(),
        ))
    }

    /// Whether the charger is currently plugged in. Re-queried at boot
    /// and updated live from the netlink monitor.
    async fn is_ac_connected(&self) -> zbus::fdo::Result<bool> {
        Ok(self.state_rx.borrow().is_ac_connected)
    }

    /// Set the ACPI platform/cooling profile manually
    /// (`power-saver`, `balanced`, `performance`, or a custom vendor
    /// string). Flips `auto_cooling` to `false` for the rest of the
    /// session — re-enable with [`set_fan_auto`](Self::set_fan_auto).
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile` (`auth_admin_keep`).
    async fn set_profile(
        &self,
        profile: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        let profile_enum = ProfileName::from_str(profile).map_err(zbus::fdo::Error::InvalidArgs)?;
        if self
            .tx
            .send(Transition::SetProfile(profile_enum))
            .await
            .is_err()
        {
            return Err(executor_down());
        }
        Ok(())
    }

    /// Unified cooling lever: set the cooling level (`silent`,
    /// `balanced`, `aggressive`), which programs the matching platform
    /// profile *and* fan curve together and latches manual cooling.
    ///
    /// This is the front-end for `hpdctl cool set`. The raw `set_profile`
    /// and `set_fan_curve` methods remain for advanced callers.
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile` (`auth_admin_keep`).
    async fn set_cooling_level(
        &self,
        level: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set Cooling Level: {}", level);
        let preset = FanCurvePreset::from_str(level)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        if self
            .tx
            .send(Transition::SetCoolingLevel(preset))
            .await
            .is_err()
        {
            return Err(executor_down());
        }
        Ok(())
    }

    /// Re-enable auto-cooling: the daemon resumes inferring the
    /// platform profile from the active TDP envelope.
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile`.
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

    /// Program a named custom fan curve (`silent`, `balanced`,
    /// `aggressive`). The daemon resolves the preset to the model's
    /// concrete curve, writes it to the EC, and re-applies it across
    /// suspend/resume.
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-fan-curve` (`auth_admin_keep`).
    async fn set_fan_curve(
        &self,
        preset: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set Fan Curve: {}", preset);
        let preset = FanCurvePreset::from_str(preset)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        if !polkit::check(conn, &header, PolkitAction::SetFanCurve).await {
            return Err(auth_denied());
        }
        if self
            .tx
            .send(Transition::SetFanCurve(FanCurveSelection::Preset(preset)))
            .await
            .is_err()
        {
            return Err(executor_down());
        }
        Ok(())
    }

    /// Hand fan control back to the firmware's automatic curve.
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-fan-curve`.
    async fn reset_fan_curve(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to reset fan curve to firmware auto");
        if !polkit::check(conn, &header, PolkitAction::SetFanCurve).await {
            return Err(auth_denied());
        }
        if self.tx.send(Transition::ResetFanCurve).await.is_err() {
            return Err(executor_down());
        }
        Ok(())
    }

    /// Live thermal telemetry as a 4-tuple of whole units:
    /// `(cpu_temp_c, gpu_temp_c, cpu_fan_rpm, gpu_fan_rpm)`.
    ///
    /// Read on demand straight from the backend, so callers
    /// (`hpdctl status` / `monitor`) always see current values. Any
    /// field equals `i32::MIN` when that sensor or fan is not exposed by
    /// the hardware (or its read failed) — note `0` is a *valid* fan
    /// reading (a stopped fan), so absence needs its own sentinel.
    async fn get_thermal_status(&self) -> (i32, i32, i32, i32) {
        let cpu_temp = self
            .backend
            .thermal()
            .and_then(|t| t.get_cpu_temp().ok().flatten())
            .map_or(TELEMETRY_UNAVAILABLE, |c| i32::from(c.0));
        let gpu_temp = self
            .backend
            .thermal()
            .and_then(|t| t.get_gpu_temp().ok().flatten())
            .map_or(TELEMETRY_UNAVAILABLE, |c| i32::from(c.0));
        let cpu_rpm = self
            .backend
            .fan()
            .and_then(|f| f.get_cpu_fan_rpm().ok())
            .map_or(TELEMETRY_UNAVAILABLE, |r| i32::from(r.0));
        let gpu_rpm = self
            .backend
            .fan()
            .and_then(|f| f.get_gpu_fan_rpm().ok().flatten())
            .map_or(TELEMETRY_UNAVAILABLE, |r| i32::from(r.0));
        (cpu_temp, gpu_temp, cpu_rpm, gpu_rpm)
    }

    /// Active fan-curve selection: a preset name (`silent`, `balanced`,
    /// `aggressive`), `custom` for an explicit curve, or `auto` when the
    /// firmware's automatic curve is in charge (daemon not managing it).
    #[zbus(property)]
    async fn fan_curve(&self) -> String {
        match self.state_rx.borrow().active_fan_curve {
            None => "auto".to_string(),
            Some(FanCurveSelection::Preset(p)) => p.as_str().to_string(),
            Some(FanCurveSelection::Custom { .. }) => "custom".to_string(),
        }
    }
}
