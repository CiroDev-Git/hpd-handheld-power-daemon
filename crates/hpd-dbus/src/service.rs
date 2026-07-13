// SPDX-License-Identifier: GPL-3.0-or-later

// zbus's `#[interface]` macro synthesises items (interface `name()`
// shim, `*_changed` signal emitters for properties) whose docs we
// can't attach via `///`. Suppress the lint module-wide; every
// human-written method in here is documented individually.
#![allow(missing_docs)]

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error};
use zbus::interface;
use zbus::zvariant::{OwnedValue, Value};

use hpd_capabilities::backend::HwBackend;
use hpd_capabilities::fan_curve::{
    FanCurve, FanCurvePoint, FanCurvePreset, FanCurveSelection, FAN_CURVE_POINTS,
};
use hpd_capabilities::gpu_clock::{GpuClockRange, GpuClockSelection};
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
    /// Whether `hpd_dbus::ppd_shim` actually claimed
    /// `net.hadess.PowerProfiles`. Shared with the daemon's startup code
    /// (an `Arc` rather than a plain `bool`) because whether the claim
    /// succeeds is only known *after* this interface object already had
    /// to exist (the object server needs it before `request_name` can be
    /// attempted) — the daemon flips this once it has an answer.
    ppd_shim_active: Arc<AtomicBool>,
}

impl PowerDaemonInterface {
    /// Build the interface from the daemon's wiring. `tx` is the
    /// command lane into the [`Executor`](hpd_core::executor::Executor);
    /// `state_rx` is the live state mirror property getters read from;
    /// `limits` is the immutable hardware envelope detected at startup;
    /// `backend` is the shared handle used for live telemetry reads;
    /// `ppd_shim_active` is flipped by the daemon once it knows whether
    /// the `net.hadess.PowerProfiles` compat shim actually claimed its
    /// name (see the field doc).
    pub fn new(
        tx: mpsc::Sender<Transition>,
        state_rx: watch::Receiver<ProfileState>,
        limits: PowerEnvelopeLimits,
        backend: Arc<dyn HwBackend>,
        ppd_shim_active: Arc<AtomicBool>,
    ) -> Self {
        Self {
            tx,
            state_rx,
            limits,
            backend,
            ppd_shim_active,
        }
    }

    /// Forward a [`Transition`] into the executor, mapping a closed
    /// command channel (executor gone) to the standard `executor_down()`
    /// D-Bus error. Every polkit-gated setter funnels through here so the
    /// "send or report executor down" boilerplate lives in one place.
    async fn send(&self, transition: Transition) -> zbus::fdo::Result<()> {
        if self.tx.send(transition).await.is_err() {
            error!("Failed to send transition to executor");
            return Err(executor_down());
        }
        Ok(())
    }

    /// Reject a power/cooling write while the "AC = maximum performance"
    /// lock is active (`ac_locked`), giving the caller an immediate, clear
    /// error instead of a silently-ignored command. The reducer enforces
    /// the same rule as a backstop. The battery charge setter deliberately
    /// does **not** call this — it stays editable on AC.
    fn reject_if_locked(&self) -> zbus::fdo::Result<()> {
        if self.state_rx.borrow().ac_locked {
            return Err(locked_on_ac());
        }
        Ok(())
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

/// Insert `key` into an `a{sv}` map only when `value` is `Some`. Used by
/// both `get_telemetry` (absence = "this hardware does not expose this
/// reading") and `get_fan_curve_constraints` (absence = "no programmable
/// fan curve at all") — never a placeholder value either way. Silently
/// drops a key on the (practically unreachable, for the plain scalar/
/// string/array types this is called with) `OwnedValue` conversion
/// failure rather than letting one bad field take down the whole map.
fn insert_dbus_value<T>(map: &mut HashMap<String, OwnedValue>, key: &'static str, value: Option<T>)
where
    Value<'static>: From<T>,
{
    if let Some(v) = value {
        if let Ok(owned) = OwnedValue::try_from(Value::<'static>::from(v)) {
            map.insert(key.to_string(), owned);
        }
    }
}

/// Build a [`FanCurve`] from the raw `(temp_c, pwm)` pairs a D-Bus caller
/// sent, rejecting anything but exactly [`FAN_CURVE_POINTS`] of them.
/// Monotonicity/range/safety-floor validation happens separately (see
/// `set_fan_curve`) — this only fixes the shape.
fn parse_fan_curve(points: Vec<(u8, u8)>) -> Result<FanCurve, zbus::fdo::Error> {
    let len = points.len();
    let array: [(u8, u8); FAN_CURVE_POINTS] = points.try_into().map_err(|_| {
        zbus::fdo::Error::InvalidArgs(format!(
            "fan curve must have exactly {FAN_CURVE_POINTS} (temp_c, pwm) points, got {len}"
        ))
    })?;
    Ok(FanCurve::new(
        array.map(|(temp_c, pwm)| FanCurvePoint::new(temp_c, pwm)),
    ))
}

fn locked_on_ac() -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(
        "Locked: on AC power the device is pinned to maximum performance \
         (Performance / Max TDP / Aggressive). Unplug to adjust \
         (the battery charge limit stays editable)."
            .into(),
    )
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
        self.reject_if_locked()?;
        if !polkit::check(conn, &header, PolkitAction::SetTdp).await {
            return Err(auth_denied());
        }
        self.send(Transition::SetSpl(watts)).await
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
        self.reject_if_locked()?;
        if !polkit::check(conn, &header, PolkitAction::SetTdp).await {
            return Err(auth_denied());
        }
        let preset = preset_name
            .parse::<hpd_capabilities::profile::TdpPreset>()
            .map_err(zbus::fdo::Error::InvalidArgs)?;
        self.send(Transition::SetPreset(preset)).await
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
        self.send(Transition::ChargeThresholdChanged(threshold))
            .await
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

    /// Whether the daemon is currently inferring the **fan curve** from
    /// the TDP envelope (auto cooling) or honouring the operator's last
    /// explicit `set_cooling_level` (manual cooling).
    ///
    /// Backed by `ProfileState::fan_follows_tdp`. Exposed on D-Bus so
    /// status widgets (KDE / GNOME power applets, overlay HUDs) can
    /// surface the mode without inferring it from observed behaviour.
    /// Re-enable auto cooling with `set_fan_auto`; `set_cooling_level`
    /// flips it back to `false`. The power-profile lever `set_profile` is
    /// decoupled and does **not** affect this flag.
    #[zbus(property)]
    async fn auto_cooling(&self) -> bool {
        self.state_rx.borrow().fan_follows_tdp
    }

    /// The daemon's own version (the crate's `CARGO_PKG_VERSION`, e.g.
    /// `2.4.2`). Read-only, unauthenticated — lets a client (the Decky
    /// plugin) show which daemon it's talking to. A client predating this
    /// method gets a D-Bus error and should fall back to "unknown".
    async fn get_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
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
    ///
    /// Kept as a method for backwards compatibility; prefer the
    /// [`ac_connected`](Self::ac_connected) property, which emits
    /// `PropertiesChanged` so clients can react without polling.
    async fn is_ac_connected(&self) -> zbus::fdo::Result<bool> {
        Ok(self.state_rx.borrow().is_ac_connected)
    }

    /// Whether the charger is currently plugged in — as a **property**, so
    /// it emits `PropertiesChanged` on every AC plug/unplug edge (the
    /// `is_ac_connected()` method above does not). Lets clients (the Decky
    /// plugin) drop their AC poll. Re-queried at boot, updated live from
    /// the netlink monitor.
    #[zbus(property)]
    async fn ac_connected(&self) -> bool {
        self.state_rx.borrow().is_ac_connected
    }

    /// Whether the power/cooling controls are currently **locked** because
    /// the device is on AC with `ac_max_performance` enabled — pinned to
    /// Performance / Max TDP / Aggressive. While `true`, `set_spl`,
    /// `set_preset`, `set_profile`, `set_cooling_level`, `set_fan_auto` and
    /// `reset_fan_curve` all fail fast with a "locked on AC" error; the
    /// battery `set_charge_threshold` stays editable. Clients (the Decky
    /// plugin) read this to disable their controls. Emits `PropertiesChanged`
    /// on every plug/unplug edge (and on a live `ac_max_performance` toggle).
    #[zbus(property)]
    async fn ac_locked(&self) -> bool {
        self.state_rx.borrow().ac_locked
    }

    /// The **"lock to maximum performance on AC"** preference (toggleable,
    /// persisted). `true` (default) = plugging in pins Performance / Max /
    /// Aggressive and locks the controls; `false` = AC is fully manual.
    /// Distinct from `AcLocked`, which is the *live* lock state
    /// (`ac_max_performance && on AC`). Emits `PropertiesChanged` when toggled.
    #[zbus(property)]
    async fn ac_max_performance(&self) -> bool {
        self.state_rx.borrow().ac_max_performance
    }

    /// Toggle the "lock to maximum performance on AC" preference. Applied
    /// immediately: enabling while plugged in forces max + locks; disabling
    /// while plugged in restores your battery state and unlocks. **Not**
    /// rejected while locked — this is how you release the lock.
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile` (`auth_admin_keep`).
    async fn set_ac_max_performance(
        &self,
        enabled: bool,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!(
            "D-Bus received request to set ac_max_performance: {}",
            enabled
        );
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        self.send(Transition::SetAcMaxPerformance(enabled)).await
    }

    /// Set the ACPI `platform_profile` manually (`power-saver`,
    /// `balanced`, `performance`, or a custom vendor string) — the EPP /
    /// power-bias lever, surfaced to users as "Power mode". Decoupled from
    /// cooling: it does **not** touch the fan curve or the `auto_cooling`
    /// flag (use `set_cooling_level` / `set_fan_auto` for those).
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile` (`auth_admin_keep`).
    async fn set_profile(
        &self,
        profile: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        self.reject_if_locked()?;
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        let profile_enum = ProfileName::from_str(profile).map_err(zbus::fdo::Error::InvalidArgs)?;
        self.send(Transition::SetProfile(profile_enum)).await
    }

    /// Cooling lever: set the fan-curve level (`silent`, `balanced`,
    /// `aggressive`) and latch manual cooling. Decoupled from power — it
    /// programs the fan curve only and does **not** touch the platform
    /// profile / power envelope (use `set_spl` / `set_profile` for those).
    ///
    /// This is the front-end for `hpdctl cool set`. The raw `set_profile`
    /// method remains for advanced callers (the unused raw `set_fan_curve`
    /// was retired in 2.5.0 — `set_cooling_level` covers the fan curve).
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile` (`auth_admin_keep`).
    async fn set_cooling_level(
        &self,
        level: &str,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to Set Cooling Level: {}", level);
        self.reject_if_locked()?;
        let preset = FanCurvePreset::from_str(level)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        self.send(Transition::SetCoolingLevel(preset)).await
    }

    /// Re-enable auto-cooling: the daemon resumes inferring the **fan
    /// curve** from the active TDP envelope (the platform profile is
    /// unaffected — it is a decoupled power lever).
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile`.
    async fn set_fan_auto(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        self.reject_if_locked()?;
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        self.send(Transition::EnableFanAuto).await
    }

    /// Hand fan control back to the firmware's automatic curve.
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile` (grouped with the
    /// other cooling levers `set_cooling_level` / `set_fan_auto`; the
    /// dedicated `set-fan-curve` action was retired in 2.5.0 along with
    /// the unused raw `set_fan_curve` method).
    async fn reset_fan_curve(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to reset fan curve to firmware auto");
        self.reject_if_locked()?;
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        self.send(Transition::ResetFanCurve).await
    }

    /// Program an explicit, hand-drawn 8-point curve for each fan
    /// (daemon ≥ 2.9.0) and latch manual cooling — the custom-curve
    /// counterpart of `set_cooling_level`'s named presets. Each of `cpu`
    /// / `gpu` is exactly 8 `(temp_c, pwm)` pairs.
    ///
    /// Validated **twice**: here, against this device's
    /// `get_fan_curve_constraints` (temperatures strictly increasing,
    /// duty non-decreasing, in-range, at/above the safety floor), and
    /// again independently by the L1 backend right before it writes to
    /// the EC. A violation returns `InvalidArgs` naming the offending
    /// point.
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile` (same bucket as the
    /// other cooling levers).
    async fn set_fan_curve(
        &self,
        cpu: Vec<(u8, u8)>,
        gpu: Vec<(u8, u8)>,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to set a custom fan curve");
        self.reject_if_locked()?;

        let Some(curve_cap) = self.backend.fan_curve() else {
            return Err(zbus::fdo::Error::Failed(
                "this device has no programmable fan curve".into(),
            ));
        };
        let cpu_curve = parse_fan_curve(cpu)?;
        let gpu_curve = parse_fan_curve(gpu)?;
        let constraints = curve_cap.constraints();
        cpu_curve
            .validate_against(&constraints)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(format!("cpu curve, {e}")))?;
        gpu_curve
            .validate_against(&constraints)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(format!("gpu curve, {e}")))?;

        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        self.send(Transition::SetCustomFanCurve {
            cpu: cpu_curve,
            gpu: gpu_curve,
        })
        .await
    }

    /// This device's fan-curve limits and safety floor (daemon ≥ 2.9.0),
    /// so a client (the plugin's curve editor, `hpdctl cool set-custom`)
    /// can validate a hand-drawn curve precisely for the running device
    /// instead of guessing. Empty map when the device has no
    /// programmable fan curve at all.
    ///
    /// Keys: `points` (`u`, always [`FAN_CURVE_POINTS`]), `temp_min_c` /
    /// `temp_max_c` (`y`), `pwm_min` / `pwm_max` (`y`), `safety_floor`
    /// (`a(yy)`, `(temp_threshold_c, min_pwm)` pairs — at or above the
    /// threshold, `pwm` must be at least `min_pwm`).
    async fn get_fan_curve_constraints(&self) -> HashMap<String, OwnedValue> {
        let mut map = HashMap::new();
        let Some(curve_cap) = self.backend.fan_curve() else {
            return map;
        };
        let c = curve_cap.constraints();
        insert_dbus_value(&mut map, "points", Some(FAN_CURVE_POINTS as u32));
        insert_dbus_value(&mut map, "temp_min_c", Some(c.temp_min_c));
        insert_dbus_value(&mut map, "temp_max_c", Some(c.temp_max_c));
        insert_dbus_value(&mut map, "pwm_min", Some(c.pwm_min));
        insert_dbus_value(&mut map, "pwm_max", Some(c.pwm_max));
        insert_dbus_value(&mut map, "safety_floor", Some(c.safety_floor));
        map
    }

    /// Live thermal telemetry:
    /// `(cpu_temp_c, gpu_temp_c, cpu_fan_rpm, gpu_fan_rpm, soc_power_mw)`.
    /// The first four are whole units (°C, RPM); the last is the actual
    /// SoC power draw in **milliwatts**.
    ///
    /// Read on demand straight from the backend, so callers
    /// (`hpdctl status` / `monitor`) always see current values. Any
    /// field equals `i32::MIN` when that sensor or fan is not exposed by
    /// the hardware (or its read failed) — note `0` is a *valid* fan
    /// reading (a stopped fan), so absence needs its own sentinel.
    async fn get_thermal_status(&self) -> (i32, i32, i32, i32, i32) {
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
        let soc_power_mw = self
            .backend
            .thermal()
            .and_then(|t| t.get_soc_power().ok().flatten())
            .map_or(TELEMETRY_UNAVAILABLE, |p| {
                i32::try_from(p.0).unwrap_or(i32::MAX)
            });
        (cpu_temp, gpu_temp, cpu_rpm, gpu_rpm, soc_power_mw)
    }

    /// Extended telemetry as an open-ended `a{sv}` map. A key is present
    /// only when the hardware actually exposes that reading — absence
    /// (not a placeholder value) means "not supported on this device",
    /// so clients should treat a missing key exactly like a hidden
    /// control. Prefer this over `get_thermal_status` in new code; that
    /// method stays only for backward compatibility with older clients.
    ///
    /// Keys (all independently optional): `cpu_temp_c` / `gpu_temp_c`
    /// (`i`, °C), `cpu_fan_rpm` / `gpu_fan_rpm` (`u`, RPM), `soc_power_mw`
    /// (`u`, mW), `battery_power_mw` (`u`, mW — only while discharging),
    /// `battery_percent` (`u`, 0-100), `battery_status` (`s`, raw kernel
    /// string), `battery_health_pct` (`u`), `battery_cycles` (`u`),
    /// `cpu_freq_mhz` / `gpu_freq_mhz` (`u`), `gpu_busy_pct` (`u`,
    /// 0-100), `cpu_busy_pct` (`u`, 0-100 — averaged over the interval
    /// since the previous call, absent on the first call after daemon
    /// start), `vram_used_mb` / `vram_total_mb` (`u`),
    /// `gpu_throttle_status` (`t`, raw bitmask — no current backend
    /// populates this).
    async fn get_telemetry(&self) -> HashMap<String, OwnedValue> {
        let mut map = HashMap::new();

        if let Some(thermal) = self.backend.thermal() {
            insert_dbus_value(
                &mut map,
                "cpu_temp_c",
                thermal
                    .get_cpu_temp()
                    .ok()
                    .flatten()
                    .map(|c| i32::from(c.0)),
            );
            insert_dbus_value(
                &mut map,
                "gpu_temp_c",
                thermal
                    .get_gpu_temp()
                    .ok()
                    .flatten()
                    .map(|c| i32::from(c.0)),
            );
            insert_dbus_value(
                &mut map,
                "soc_power_mw",
                thermal.get_soc_power().ok().flatten().map(|p| p.0),
            );
        }
        if let Some(fan) = self.backend.fan() {
            insert_dbus_value(
                &mut map,
                "cpu_fan_rpm",
                fan.get_cpu_fan_rpm().ok().map(|r| u32::from(r.0)),
            );
            insert_dbus_value(
                &mut map,
                "gpu_fan_rpm",
                fan.get_gpu_fan_rpm().ok().flatten().map(|r| u32::from(r.0)),
            );
        }
        if let Some(t) = self.backend.telemetry() {
            insert_dbus_value(
                &mut map,
                "battery_power_mw",
                t.get_battery_power().ok().flatten().map(|p| p.0),
            );
            insert_dbus_value(
                &mut map,
                "battery_percent",
                t.get_battery_percent().ok().flatten().map(u32::from),
            );
            insert_dbus_value(
                &mut map,
                "battery_status",
                t.get_battery_status().ok().flatten(),
            );
            insert_dbus_value(
                &mut map,
                "battery_health_pct",
                t.get_battery_health_pct().ok().flatten().map(u32::from),
            );
            insert_dbus_value(
                &mut map,
                "battery_cycles",
                t.get_battery_cycles().ok().flatten(),
            );
            insert_dbus_value(
                &mut map,
                "cpu_freq_mhz",
                t.get_cpu_freq_mhz().ok().flatten(),
            );
            insert_dbus_value(
                &mut map,
                "gpu_freq_mhz",
                t.get_gpu_freq_mhz().ok().flatten(),
            );
            insert_dbus_value(
                &mut map,
                "gpu_busy_pct",
                t.get_gpu_busy_pct().ok().flatten().map(u32::from),
            );
            insert_dbus_value(
                &mut map,
                "cpu_busy_pct",
                t.get_cpu_busy_pct().ok().flatten().map(u32::from),
            );
            insert_dbus_value(
                &mut map,
                "vram_used_mb",
                t.get_vram_used_mb().ok().flatten(),
            );
            insert_dbus_value(
                &mut map,
                "vram_total_mb",
                t.get_vram_total_mb().ok().flatten(),
            );
            insert_dbus_value(
                &mut map,
                "gpu_throttle_status",
                t.get_gpu_throttle_status().ok().flatten(),
            );
        }

        map
    }

    /// The eight `(temp_c, pwm)` points of the active CPU and GPU fan
    /// curves as read back from the EC: `(cpu_points, gpu_points)`. `pwm`
    /// is 0–255. Both vectors are empty when the backend exposes no
    /// programmable curve or the read-back failed. Used by
    /// `hpdctl cool curve` to draw the curve.
    async fn get_fan_curve(&self) -> (Vec<(u32, u32)>, Vec<(u32, u32)>) {
        let Some(cap) = self.backend.fan_curve() else {
            return (Vec::new(), Vec::new());
        };
        match cap.get_curves() {
            Ok(curves) => {
                let to_pairs = |c: &hpd_capabilities::fan_curve::FanCurve| {
                    c.points
                        .iter()
                        .map(|p| (u32::from(p.temp_c), u32::from(p.pwm)))
                        .collect::<Vec<_>>()
                };
                (to_pairs(&curves.cpu), to_pairs(&curves.gpu))
            }
            Err(_) => (Vec::new(), Vec::new()),
        }
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

    /// Manual override: program an explicit GPU clock frequency range
    /// (`min_mhz`, `max_mhz`) and disable `gpu_follows_tdp` — the GPU
    /// counterpart of `set_fan_curve`'s hand-drawn curve.
    ///
    /// Validated **twice**: here, against this device's LIVE
    /// `get_gpu_clock_constraints` (the kernel-reported OD_RANGE — Class A
    /// data, not a per-model calibration), and again independently by the
    /// L1 backend right before it writes to the hardware.
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile` (same bucket as the
    /// other cooling/performance levers).
    async fn set_gpu_clock_range(
        &self,
        min_mhz: u32,
        max_mhz: u32,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!(
            "D-Bus received request to set GPU clock range: {}-{}MHz",
            min_mhz, max_mhz
        );
        self.reject_if_locked()?;

        let Some(gpu_cap) = self.backend.gpu_clock() else {
            return Err(zbus::fdo::Error::Failed(
                "this device has no programmable GPU clock range".into(),
            ));
        };
        let range = GpuClockRange { min_mhz, max_mhz };
        let constraints = gpu_cap.constraints().map_err(|e| {
            zbus::fdo::Error::Failed(format!("could not read GPU clock constraints: {e}"))
        })?;
        range
            .validate_against(&constraints)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;

        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        self.send(Transition::SetGpuClockRange { min_mhz, max_mhz })
            .await
    }

    /// Re-enable GPU-clock auto-follow: the daemon resumes inferring a GPU
    /// clock ceiling from the active TDP envelope (mirrors `set_fan_auto`
    /// for the fan curve). Immediately infers and applies a range for the
    /// current SPL rather than waiting for the next TDP change.
    ///
    /// This is the opt-in the whole feature is gated behind — the daemon
    /// never touches the GPU clock until this is called at least once
    /// (see `ProfileState::active_gpu_clock`'s docs on why the default is
    /// permanently off, unlike the fan curve's auto-cooling).
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile`.
    async fn enable_gpu_auto_follow(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        self.reject_if_locked()?;
        if self.backend.gpu_clock().is_none() {
            return Err(zbus::fdo::Error::Failed(
                "this device has no programmable GPU clock range".into(),
            ));
        }
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        self.send(Transition::EnableGpuAutoFollow).await
    }

    /// Hand the GPU clock back to firmware auto.
    ///
    /// `polkit` action: `dev.cirodev.hpd.set-profile` (grouped with the
    /// other cooling/performance levers, mirrors `reset_fan_curve`).
    async fn reset_gpu_clocks(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to reset GPU clocks to firmware auto");
        self.reject_if_locked()?;
        if !polkit::check(conn, &header, PolkitAction::SetProfile).await {
            return Err(auth_denied());
        }
        self.send(Transition::ResetGpuClocks).await
    }

    /// Restore recommended defaults in one daemon transaction: TDP ->
    /// Balanced, Power mode -> Performance, Charge cap -> 100%, Cooling ->
    /// firmware auto, and GPU clock -> firmware auto (only if already
    /// opted into a custom range — never auto-opts a user in).
    ///
    /// `polkit` actions: `dev.cirodev.hpd.set-tdp` AND
    /// `dev.cirodev.hpd.set-charge` AND `dev.cirodev.hpd.set-profile` —
    /// all three, since this bundles levers each individually gates; a
    /// caller authorized for only one of them shouldn't get the other two
    /// for free through this composite door. No new polkit action.
    async fn restore_defaults(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
        #[zbus(header)] header: zbus::message::Header<'_>,
    ) -> zbus::fdo::Result<()> {
        debug!("D-Bus received request to restore recommended defaults");
        self.reject_if_locked()?;
        let authorized = polkit::check(conn, &header, PolkitAction::SetTdp).await
            && polkit::check(conn, &header, PolkitAction::SetCharge).await
            && polkit::check(conn, &header, PolkitAction::SetProfile).await;
        if !authorized {
            return Err(auth_denied());
        }
        self.send(Transition::RestoreDefaults).await
    }

    /// This device's GPU clock range bounds — the kernel-reported LIVE
    /// `OD_RANGE` (Class A data: a generic kernel interface, not a
    /// per-model calibration, so it needs no recalibration on other
    /// hardware), so a client (the plugin's range editor) can validate a
    /// custom range precisely for the running device instead of guessing.
    /// Empty map when the device has no programmable GPU clock range at
    /// all, or the live read failed.
    ///
    /// Keys: `range_min_mhz` / `range_max_mhz` (`u`).
    async fn get_gpu_clock_constraints(&self) -> HashMap<String, OwnedValue> {
        let mut map = HashMap::new();
        let Some(gpu_cap) = self.backend.gpu_clock() else {
            return map;
        };
        match gpu_cap.constraints() {
            Ok(c) => {
                insert_dbus_value(&mut map, "range_min_mhz", Some(c.range_min_mhz));
                insert_dbus_value(&mut map, "range_max_mhz", Some(c.range_max_mhz));
            }
            Err(e) => debug!(error = %e, "Could not read live GPU clock constraints"),
        }
        map
    }

    /// The GPU clock range `(min_mhz, max_mhz)` currently committed to the
    /// hardware, read back from the backend exactly like `get_fan_curve`.
    /// `(0, 0)` when the backend has no `GpuClockRangeControl`, the daemon
    /// is not managing GPU clocks (firmware auto), or the read failed — 0
    /// MHz is never a real value on any exposed hardware, so it doubles as
    /// an unambiguous "not applicable" sentinel (mirrors
    /// `TELEMETRY_UNAVAILABLE`).
    async fn get_gpu_clock_range(&self) -> (u32, u32) {
        let Some(gpu_cap) = self.backend.gpu_clock() else {
            return (0, 0);
        };
        match gpu_cap.active_range() {
            Ok(Some(range)) => (range.min_mhz, range.max_mhz),
            _ => (0, 0),
        }
    }

    /// Active GPU-clock selection: a preset name (`silent`, `balanced`,
    /// `aggressive`), `custom` for an explicit range, or `auto` when the
    /// firmware is in charge (the daemon never touches GPU clocks until
    /// `enable_gpu_auto_follow` / `set_gpu_clock_range` is called at least
    /// once). Mirrors the `fan_curve` property.
    #[zbus(property)]
    async fn gpu_clock_range(&self) -> String {
        match self.state_rx.borrow().active_gpu_clock {
            None => "auto".to_string(),
            Some(GpuClockSelection::Preset(p)) => p.as_str().to_string(),
            Some(GpuClockSelection::Custom(_)) => "custom".to_string(),
        }
    }

    /// Whether the daemon is currently inferring the GPU clock ceiling
    /// from the TDP envelope (mirrors `auto_cooling` for the fan curve).
    /// `false` is the permanent default until `enable_gpu_auto_follow` /
    /// `set_gpu_clock_range` is called at least once.
    #[zbus(property)]
    async fn gpu_follows_tdp(&self) -> bool {
        self.state_rx.borrow().gpu_follows_tdp
    }

    /// Daemon self-diagnostics: `(polkit_ok, missing_action_ids)`.
    ///
    /// `polkit_ok == false` (a non-empty `missing_action_ids`) means the
    /// polkit policy was never installed, so every privileged command is
    /// denied with `AuthFailed`. `hpdctl status` / `hpdctl doctor` and the
    /// Decky plugin render this to point the user at `hpdctl fix-polkit`.
    ///
    /// A transport error talking to polkit is reported as "ok" (empty
    /// missing list): the loud startup self-check already covers that
    /// rarer case, and we would rather not cry wolf over a transient
    /// polkit hiccup on every status poll.
    async fn get_diagnostics(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> (bool, Vec<String>) {
        let missing: Vec<String> = polkit::missing_actions(conn)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(String::from)
            .collect();
        (missing.is_empty(), missing)
    }

    /// Friendly names of competing power daemons currently live on the bus
    /// (e.g. `power-profiles-daemon`, `steamos-manager`).
    ///
    /// These write the same TDP / platform-profile / charge surfaces hpd
    /// owns; running one alongside hpd makes the effective state flap. An
    /// empty list means hpd is the sole power owner. The repair is the
    /// user-side `hpdctl doctor` run in fix mode (the `fix` subcommand
    /// flag) — the daemon is sandboxed and cannot disable another service
    /// itself. See [`crate::conflicts`].
    //
    // NOTE: keep this doc-comment free of `--` (two ASCII hyphens). zbus
    // copies each `///` line verbatim into the introspection `<!-- ... -->`
    // block, and XML forbids `--` inside a comment; a stray `--fix` here
    // produced a malformed document that strict parsers (Python expat, used
    // by the Decky plugin's dbus-next) rejected outright. The
    // `introspection_xml_is_well_formed` regression test guards this.
    async fn get_power_conflicts(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Vec<String> {
        crate::conflicts::power_conflicts(conn).await
    }

    /// Friendly names of power-adjacent advisory daemons currently live on
    /// the bus (today Feral `gamemoded`, activated by Steam / Lutris around
    /// a running game).
    ///
    /// Unlike `get_power_conflicts`, these are not rivals to neutralize:
    /// they may raise the CPU governor while a game runs, which is wanted,
    /// so `hpdctl doctor` in fix mode reports them but never masks them. An
    /// empty list means no advisory daemon is live. The call errors against a
    /// daemon predating this method; callers degrade to "unknown". See
    /// [`crate::conflicts`].
    //
    // NOTE: keep this doc-comment free of `--` (two ASCII hyphens) for the
    // same XML-introspection reason documented on `get_power_conflicts`.
    async fn get_advisory_daemons(
        &self,
        #[zbus(connection)] conn: &zbus::Connection,
    ) -> Vec<String> {
        crate::conflicts::advisory_daemons(conn).await
    }

    /// Whether the `net.hadess.PowerProfiles` compat shim
    /// (`hpd_dbus::ppd_shim`) actually claimed its bus name at startup —
    /// `false` means a real `power-profiles-daemon`/`tuned-ppd` was live
    /// and not masked, so the KDE power applet and `game-performance`
    /// still see no owner. `hpdctl status`/`doctor` render this as
    /// "compat PPD: active/inactive". Rendered by `hpdctl status` /
    /// `hpdctl doctor`.
    async fn get_ppd_shim_active(&self) -> bool {
        self.ppd_shim_active.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use hpd_capabilities::power::{PowerEnvelopeLimits, PowerEnvelopeTarget};
    use hpd_capabilities::testing::MockBackend;
    use hpd_capabilities::units::PowerMilliwatts;
    use std::sync::Arc;
    // Brings `introspect_to_writer` into scope — it's a method on the
    // `Interface` trait the `#[interface]` macro implements for us.
    use zbus::object_server::Interface;

    fn sample_state() -> ProfileState {
        ProfileState {
            power_target: PowerEnvelopeTarget {
                spl: PowerMilliwatts(15000),
                sppt: PowerMilliwatts(17000),
                fppt: Some(PowerMilliwatts(20000)),
            },
            active_profile: hpd_capabilities::profile::ProfileName::Balanced,
            charge_end_threshold: 80,
            fan_follows_tdp: true,
            last_dc_state: None,
            active_fan_curve: None,
            active_gpu_clock: None,
            gpu_follows_tdp: false,
            is_ac_connected: false,
            ac_max_performance: true,
            ac_locked: false,
        }
    }

    fn sample_limits() -> PowerEnvelopeLimits {
        PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000),
            sppt_min: PowerMilliwatts(7000),
            sppt_max: PowerMilliwatts(43000),
            fppt_min: PowerMilliwatts(7000),
            fppt_max: PowerMilliwatts(53000),
        }
    }

    /// Render the introspection document for the exported object path exactly
    /// as zbus' `ObjectServer` assembles it: the DOCTYPE + `<node>` envelope
    /// wrapping our interface's `introspect_to_writer` output (the same
    /// generation path `Introspectable.Introspect` serves at runtime). The
    /// standard `Peer` / `Properties` / `Introspectable` interfaces zbus
    /// injects carry no user doc-comments and are always well-formed, so our
    /// own interface is the only regression surface that matters.
    fn introspection_document() -> String {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let (_state_tx, state_rx) = tokio::sync::watch::channel(sample_state());
        let backend: Arc<dyn HwBackend> = Arc::new(MockBackend::new(
            sample_state().power_target,
            sample_limits(),
        ));
        let iface = PowerDaemonInterface::new(
            tx,
            state_rx,
            sample_limits(),
            backend,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );

        let mut body = String::new();
        iface.introspect_to_writer(&mut body, 2);

        format!(
            "<!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\"\n\
             \x20\"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd\">\n\
             <node>\n{body}</node>\n"
        )
    }

    /// The introspection XML for the exported object path must be well-formed
    /// under a *strict* parser. A `///` doc-comment containing `--` slips past
    /// lenient parsers (libxml2 / gdbus) but is rejected by Python's expat,
    /// which broke the Decky plugin (panel stuck on "Daemon: unreachable").
    #[test]
    fn introspection_xml_is_well_formed() {
        let xml = introspection_document();

        let mut reader = quick_xml::Reader::from_str(&xml);
        reader.config_mut().check_comments = true;

        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => {}
                Err(e) => panic!(
                    "introspection XML is not well-formed under a strict parser: {e}\n\
                     A `///` doc-comment on the D-Bus interface almost certainly contains \
                     `--`, which XML forbids inside a comment. Document:\n{xml}"
                ),
            }
        }
    }

    /// Belt-and-braces guard for the exact bug that shipped: `--` inside any
    /// `<!-- ... -->` block. Independent of the parser's default config, so a
    /// future `quick-xml` bump that changes comment handling can't silence it.
    #[test]
    fn introspection_comments_have_no_double_hyphen() {
        let xml = introspection_document();
        for (start, _) in xml.match_indices("<!--") {
            let rest = &xml[start + 4..];
            let end = rest
                .find("-->")
                .expect("every opened XML comment is closed");
            let body = &rest[..end];
            assert!(
                !body.contains("--"),
                "XML comment body contains `--`, which strict parsers reject:\n{body}"
            );
        }
    }

    /// Minimal `HwBackend` fixture exercising `get_telemetry`'s mapping:
    /// some fields overridden with fixed readings, others left at the
    /// trait's default `Ok(None)` — verifying present values are mapped
    /// to the right key/type *and* that a field the "hardware" doesn't
    /// expose is omitted entirely rather than sent as a placeholder.
    struct TelemetryFixture;

    impl hpd_capabilities::power::PowerEnvelope for TelemetryFixture {
        fn get_limits(&self) -> Result<PowerEnvelopeLimits, hpd_error::HpdError> {
            Ok(sample_limits())
        }
        fn get_target(&self) -> Result<PowerEnvelopeTarget, hpd_error::HpdError> {
            Ok(sample_state().power_target)
        }
        fn set_target(&self, _target: &PowerEnvelopeTarget) -> Result<(), hpd_error::HpdError> {
            Ok(())
        }
    }

    impl hpd_capabilities::thermal::ThermalSensors for TelemetryFixture {
        fn get_cpu_temp(
            &self,
        ) -> Result<Option<hpd_capabilities::units::Celsius>, hpd_error::HpdError> {
            Ok(Some(hpd_capabilities::units::Celsius(65)))
        }
        fn get_gpu_temp(
            &self,
        ) -> Result<Option<hpd_capabilities::units::Celsius>, hpd_error::HpdError> {
            Ok(None) // no distinct GPU sensor on this fixture "device"
        }
        fn get_soc_power(&self) -> Result<Option<PowerMilliwatts>, hpd_error::HpdError> {
            Ok(Some(PowerMilliwatts(18300)))
        }
    }

    impl hpd_capabilities::fan::FanControl for TelemetryFixture {
        fn get_cpu_fan_rpm(&self) -> Result<hpd_capabilities::units::Rpm, hpd_error::HpdError> {
            Ok(hpd_capabilities::units::Rpm(4200))
        }
        fn get_gpu_fan_rpm(
            &self,
        ) -> Result<Option<hpd_capabilities::units::Rpm>, hpd_error::HpdError> {
            Ok(None) // single-fan fixture
        }
    }

    impl hpd_capabilities::telemetry::SystemTelemetry for TelemetryFixture {
        fn get_battery_power(&self) -> Result<Option<PowerMilliwatts>, hpd_error::HpdError> {
            Ok(Some(PowerMilliwatts(15000)))
        }
        fn get_battery_percent(&self) -> Result<Option<u8>, hpd_error::HpdError> {
            Ok(Some(64))
        }
        fn get_battery_status(&self) -> Result<Option<String>, hpd_error::HpdError> {
            Ok(Some("Discharging".to_string()))
        }
        // battery_health_pct, battery_cycles, cpu_freq_mhz, gpu_freq_mhz,
        // gpu_busy_pct, vram_*, gpu_throttle_status: left at the trait's
        // default `Ok(None)` on purpose (see the assertion below).
    }

    impl hpd_capabilities::fan_curve::FanCurveControl for TelemetryFixture {
        fn apply(
            &self,
            _selection: &hpd_capabilities::fan_curve::FanCurveSelection,
        ) -> Result<(), hpd_error::HpdError> {
            Ok(())
        }
        fn reset_to_auto(&self) -> Result<(), hpd_error::HpdError> {
            Ok(())
        }
        fn get_curves(
            &self,
        ) -> Result<hpd_capabilities::fan_curve::ActiveFanCurves, hpd_error::HpdError> {
            Err(hpd_error::HpdError::FeatureUnsupported)
        }
        fn active_selection(
            &self,
        ) -> Result<Option<hpd_capabilities::fan_curve::FanCurveSelection>, hpd_error::HpdError>
        {
            Ok(None)
        }
        fn constraints(&self) -> hpd_capabilities::fan_curve::FanCurveConstraints {
            hpd_capabilities::fan_curve::FanCurveConstraints {
                temp_min_c: 30,
                temp_max_c: 95,
                pwm_min: 0,
                pwm_max: 255,
                safety_floor: vec![(85, 150), (90, 200)],
            }
        }
    }

    impl hpd_capabilities::gpu_clock::GpuClockRangeControl for TelemetryFixture {
        fn set_range(
            &self,
            _range: &hpd_capabilities::gpu_clock::GpuClockRange,
        ) -> Result<(), hpd_error::HpdError> {
            Ok(())
        }
        fn reset_to_auto(&self) -> Result<(), hpd_error::HpdError> {
            Ok(())
        }
        fn active_range(
            &self,
        ) -> Result<Option<hpd_capabilities::gpu_clock::GpuClockRange>, hpd_error::HpdError>
        {
            Ok(Some(hpd_capabilities::gpu_clock::GpuClockRange {
                min_mhz: 600,
                max_mhz: 1_800,
            }))
        }
        fn constraints(
            &self,
        ) -> Result<hpd_capabilities::gpu_clock::GpuClockConstraints, hpd_error::HpdError> {
            Ok(hpd_capabilities::gpu_clock::GpuClockConstraints {
                range_min_mhz: 600,
                range_max_mhz: 2_900,
            })
        }
    }

    impl HwBackend for TelemetryFixture {
        fn power(&self) -> &dyn hpd_capabilities::power::PowerEnvelope {
            self
        }
        fn thermal(&self) -> Option<&dyn hpd_capabilities::thermal::ThermalSensors> {
            Some(self)
        }
        fn fan(&self) -> Option<&dyn hpd_capabilities::fan::FanControl> {
            Some(self)
        }
        fn telemetry(&self) -> Option<&dyn hpd_capabilities::telemetry::SystemTelemetry> {
            Some(self)
        }
        fn fan_curve(&self) -> Option<&dyn hpd_capabilities::fan_curve::FanCurveControl> {
            Some(self)
        }
        fn gpu_clock(&self) -> Option<&dyn hpd_capabilities::gpu_clock::GpuClockRangeControl> {
            Some(self)
        }
    }

    fn telemetry_u32(map: &HashMap<String, OwnedValue>, key: &str) -> Option<u32> {
        map.get(key).and_then(|v| u32::try_from(v).ok())
    }

    fn telemetry_str<'a>(map: &'a HashMap<String, OwnedValue>, key: &str) -> Option<&'a str> {
        map.get(key).and_then(|v| <&str>::try_from(v).ok())
    }

    #[tokio::test]
    async fn get_telemetry_reports_present_and_omits_absent_keys() {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let (_state_tx, state_rx) = tokio::sync::watch::channel(sample_state());
        let backend: Arc<dyn HwBackend> = Arc::new(TelemetryFixture);
        let iface = PowerDaemonInterface::new(
            tx,
            state_rx,
            sample_limits(),
            backend,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );

        let telemetry = iface.get_telemetry().await;

        assert_eq!(
            telemetry
                .get("cpu_temp_c")
                .and_then(|v| i32::try_from(v).ok()),
            Some(65)
        );
        assert_eq!(telemetry_u32(&telemetry, "cpu_fan_rpm"), Some(4200));
        assert_eq!(telemetry_u32(&telemetry, "soc_power_mw"), Some(18300));
        assert_eq!(telemetry_u32(&telemetry, "battery_power_mw"), Some(15000));
        assert_eq!(telemetry_u32(&telemetry, "battery_percent"), Some(64));
        assert_eq!(
            telemetry_str(&telemetry, "battery_status"),
            Some("Discharging")
        );

        // Fields the fixture "hardware" doesn't expose must be entirely
        // absent from the map, not present with a placeholder like 0.
        for missing_key in [
            "gpu_temp_c",
            "gpu_fan_rpm",
            "battery_health_pct",
            "battery_cycles",
            "cpu_freq_mhz",
            "gpu_freq_mhz",
            "gpu_busy_pct",
            "cpu_busy_pct",
            "vram_used_mb",
            "vram_total_mb",
            "gpu_throttle_status",
        ] {
            assert!(
                !telemetry.contains_key(missing_key),
                "key `{missing_key}` should be omitted when the hardware doesn't expose it, \
                 not present with a placeholder"
            );
        }
    }

    fn telemetry_u8(map: &HashMap<String, OwnedValue>, key: &str) -> Option<u8> {
        map.get(key).and_then(|v| u8::try_from(v).ok())
    }

    fn fixture_interface() -> PowerDaemonInterface {
        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let (_state_tx, state_rx) = tokio::sync::watch::channel(sample_state());
        let backend: Arc<dyn HwBackend> = Arc::new(TelemetryFixture);
        PowerDaemonInterface::new(
            tx,
            state_rx,
            sample_limits(),
            backend,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
    }

    #[tokio::test]
    async fn get_fan_curve_constraints_reports_the_device_limits() {
        let iface = fixture_interface();
        let constraints = iface.get_fan_curve_constraints().await;

        assert_eq!(telemetry_u32(&constraints, "points"), Some(8));
        assert_eq!(telemetry_u8(&constraints, "temp_min_c"), Some(30));
        assert_eq!(telemetry_u8(&constraints, "temp_max_c"), Some(95));
        assert_eq!(telemetry_u8(&constraints, "pwm_min"), Some(0));
        assert_eq!(telemetry_u8(&constraints, "pwm_max"), Some(255));
        let floor = constraints
            .get("safety_floor")
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| Vec::<(u8, u8)>::try_from(v).ok())
            .expect("safety_floor must be present and decode as (u8,u8) pairs");
        assert_eq!(floor, vec![(85, 150), (90, 200)]);
    }

    #[tokio::test]
    async fn get_gpu_clock_constraints_reports_the_device_bounds() {
        let iface = fixture_interface();
        let constraints = iface.get_gpu_clock_constraints().await;

        assert_eq!(telemetry_u32(&constraints, "range_min_mhz"), Some(600));
        assert_eq!(telemetry_u32(&constraints, "range_max_mhz"), Some(2_900));
    }

    #[tokio::test]
    async fn get_gpu_clock_range_reads_the_active_range_back() {
        let iface = fixture_interface();
        assert_eq!(iface.get_gpu_clock_range().await, (600, 1_800));
    }

    #[tokio::test]
    async fn gpu_clock_range_property_reflects_active_selection() {
        use hpd_capabilities::fan_curve::FanCurvePreset;
        use hpd_capabilities::gpu_clock::{GpuClockRange, GpuClockSelection};

        let (tx, _rx) = tokio::sync::mpsc::channel(1);
        let mut state = sample_state();

        state.active_gpu_clock = None;
        let (_state_tx, state_rx) = tokio::sync::watch::channel(state.clone());
        let iface = PowerDaemonInterface::new(
            tx.clone(),
            state_rx,
            sample_limits(),
            Arc::new(TelemetryFixture),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        assert_eq!(iface.gpu_clock_range().await, "auto");
        assert!(!iface.gpu_follows_tdp().await);

        state.active_gpu_clock = Some(GpuClockSelection::Preset(FanCurvePreset::Aggressive));
        state.gpu_follows_tdp = true;
        let (_state_tx, state_rx) = tokio::sync::watch::channel(state.clone());
        let iface = PowerDaemonInterface::new(
            tx.clone(),
            state_rx,
            sample_limits(),
            Arc::new(TelemetryFixture),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        assert_eq!(iface.gpu_clock_range().await, "aggressive");
        assert!(iface.gpu_follows_tdp().await);

        state.active_gpu_clock = Some(GpuClockSelection::Custom(GpuClockRange {
            min_mhz: 600,
            max_mhz: 1_000,
        }));
        let (_state_tx, state_rx) = tokio::sync::watch::channel(state);
        let iface = PowerDaemonInterface::new(
            tx,
            state_rx,
            sample_limits(),
            Arc::new(TelemetryFixture),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        assert_eq!(iface.gpu_clock_range().await, "custom");
    }

    /// A curve with the wrong number of points must be rejected as
    /// `InvalidArgs` before `set_fan_curve` ever gets to validate
    /// monotonicity/range/floor or reach polkit. `set_fan_curve` itself
    /// needs a live `zbus::Connection` + `Header` to call directly (they
    /// come from the bus dispatcher in production), so this exercises
    /// its `parse_fan_curve` helper — the actual shape check — instead.
    #[test]
    fn parse_fan_curve_rejects_wrong_point_count() {
        let seven_points = vec![
            (45, 20),
            (54, 40),
            (62, 60),
            (69, 80),
            (75, 100),
            (80, 120),
            (85, 150),
        ];
        assert!(parse_fan_curve(seven_points).is_err());
    }

    #[test]
    fn parse_fan_curve_accepts_exactly_eight_points() {
        let eight_points = vec![
            (45, 20),
            (54, 40),
            (62, 60),
            (69, 80),
            (75, 100),
            (80, 120),
            (85, 150),
            (92, 200),
        ];
        let curve = parse_fan_curve(eight_points).expect("exactly 8 points must parse");
        assert_eq!(curve.points[0], FanCurvePoint::new(45, 20));
        assert_eq!(curve.points[7], FanCurvePoint::new(92, 200));
    }
}
