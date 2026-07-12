// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::HashMap;
use zbus::proxy;
use zbus::zvariant::OwnedValue;

/// The eight `(temp_c, pwm)` points of one fan's curve.
pub type CurvePoints = Vec<(u32, u32)>;

#[proxy(
    interface = "dev.cirodev.hpd.PowerDaemon1",
    default_service = "dev.cirodev.hpd.PowerDaemon1",
    default_path = "/dev/cirodev/hpd/PowerDaemon1"
)]
trait PowerDaemon {
    fn is_ac_connected(&self) -> zbus::Result<bool>;
    /// The daemon's own version string (`CARGO_PKG_VERSION`). Errors
    /// against a daemon predating this method → caller shows "unknown".
    fn get_version(&self) -> zbus::Result<String>;
    fn get_hardware_limits(&self) -> zbus::Result<(u32, u32, u32, u32)>;
    async fn set_preset(&self, preset_name: &str) -> zbus::Result<()>;
    async fn set_spl(&self, watts: u32) -> zbus::Result<()>;
    async fn set_charge_threshold(&self, threshold: u8) -> zbus::Result<()>;

    #[zbus(property)]
    fn current_spl(&self) -> zbus::Result<u32>;

    #[zbus(property)]
    fn active_profile(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn charge_end_threshold(&self) -> zbus::Result<u8>;

    /// Whether the daemon is currently in auto-cooling mode (the
    /// platform profile follows the TDP envelope). Mirror of the
    /// `auto_cooling` property added in Lote 42 of the daemon.
    #[zbus(property)]
    fn auto_cooling(&self) -> zbus::Result<bool>;

    fn set_profile(&self, profile: &str) -> zbus::Result<()>;
    fn set_fan_auto(&self) -> zbus::Result<()>;

    /// Unified cooling lever: set the cooling level (`silent`,
    /// `balanced`, `aggressive`) — programs the matching platform
    /// profile and fan curve together and latches manual cooling.
    async fn set_cooling_level(&self, level: &str) -> zbus::Result<()>;

    /// Hand fan control back to the firmware's automatic curve.
    async fn reset_fan_curve(&self) -> zbus::Result<()>;

    /// Program an explicit 8-point curve for each fan (daemon ≥ 2.9.0)
    /// and latch manual cooling. Each `Vec` must have exactly 8
    /// `(temp_c, pwm)` pairs; the daemon validates against
    /// `get_fan_curve_constraints` and returns `InvalidArgs` naming the
    /// offending point on a violation.
    async fn set_fan_curve(&self, cpu: Vec<(u8, u8)>, gpu: Vec<(u8, u8)>) -> zbus::Result<()>;

    /// This device's fan-curve limits and safety floor (daemon ≥ 2.9.0):
    /// `points`, `temp_min_c`/`temp_max_c`, `pwm_min`/`pwm_max`,
    /// `safety_floor` (`(temp_threshold_c, min_pwm)` pairs). Empty map on
    /// a device with no programmable fan curve, or an older daemon.
    fn get_fan_curve_constraints(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    /// Toggle the "lock to maximum performance on AC" preference.
    async fn set_ac_max_performance(&self, enabled: bool) -> zbus::Result<()>;

    /// The "lock to maximum performance on AC" preference (toggleable).
    #[zbus(property)]
    fn ac_max_performance(&self) -> zbus::Result<bool>;

    /// Live AC-lock state (`ac_max_performance && on AC`).
    #[zbus(property)]
    fn ac_locked(&self) -> zbus::Result<bool>;

    /// Active fan-curve selection: a preset name, `custom`, or `auto`.
    #[zbus(property)]
    fn fan_curve(&self) -> zbus::Result<String>;

    /// Live thermal telemetry: `(cpu_temp_c, gpu_temp_c, cpu_fan_rpm,
    /// gpu_fan_rpm, soc_power_mw)`. The last field is the actual SoC
    /// power draw in milliwatts. Any field is `i32::MIN` when unavailable.
    fn get_thermal_status(&self) -> zbus::Result<(i32, i32, i32, i32, i32)>;

    /// Extended telemetry (daemon ≥ 2.8.0): battery power/percent/status/
    /// health/cycles, CPU/GPU frequency, GPU load, VRAM. A key is present
    /// only when the hardware exposes that reading. Errors against an
    /// older daemon — callers degrade to what `get_thermal_status` covers.
    fn get_telemetry(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    /// The 8 `(temp_c, pwm)` points of the active CPU and GPU fan
    /// curves: `(cpu_points, gpu_points)`. Empty when no curve is
    /// programmable. Used to draw the curve.
    fn get_fan_curve(&self) -> zbus::Result<(CurvePoints, CurvePoints)>;

    /// Daemon self-diagnostics: `(polkit_ok, missing_action_ids)`.
    /// `polkit_ok == false` means the polkit policy is not installed and
    /// every privileged command will be denied with `AuthFailed`.
    fn get_diagnostics(&self) -> zbus::Result<(bool, Vec<String>)>;

    /// Friendly names of competing power daemons currently live on the
    /// system bus (`power-profiles-daemon`, `steamos-manager`). Empty when
    /// hpd is the sole power owner. Rendered by `hpdctl doctor` / `status`.
    /// The call errors against a daemon predating this method; callers
    /// degrade to "unknown / update hpd".
    fn get_power_conflicts(&self) -> zbus::Result<Vec<String>>;

    /// Friendly names of power-adjacent advisory daemons currently live on
    /// the bus (today Feral `gamemoded`). Reported, never masked. Empty when
    /// none is live. The call errors against a daemon predating this method;
    /// callers degrade to "unknown / update hpd".
    fn get_advisory_daemons(&self) -> zbus::Result<Vec<String>>;

    /// Whether the `net.hadess.PowerProfiles` compat shim (daemon ≥
    /// 2.10.0) claimed its bus name at startup — `false` means a real
    /// `power-profiles-daemon`/`tuned-ppd` was live and not masked, so
    /// the KDE power applet and `game-performance` still see no owner.
    /// The call errors against an older daemon; callers degrade to
    /// "unknown / update hpd".
    fn get_ppd_shim_active(&self) -> zbus::Result<bool>;

    /// Manual override: program an explicit GPU clock frequency range
    /// (daemon ≥ 2.12.0) and disengage auto-follow. Validated against
    /// `get_gpu_clock_constraints` both here and again independently by
    /// the daemon.
    async fn set_gpu_clock_range(&self, min_mhz: u32, max_mhz: u32) -> zbus::Result<()>;

    /// Re-enable GPU-clock auto-follow: the daemon infers a clock
    /// ceiling from the active TDP envelope, mirroring `set_fan_auto`.
    /// The opt-in the whole feature is gated behind — the daemon never
    /// touches the GPU clock until this or `set_gpu_clock_range` is
    /// called at least once.
    async fn enable_gpu_auto_follow(&self) -> zbus::Result<()>;

    /// Hand the GPU clock back to firmware auto.
    async fn reset_gpu_clocks(&self) -> zbus::Result<()>;

    /// This device's GPU clock range bounds (daemon ≥ 2.12.0): the
    /// kernel-reported live `OD_RANGE`, keys `range_min_mhz`/
    /// `range_max_mhz` (`u`). Empty map when the device has no
    /// programmable GPU clock range, or the live read failed.
    fn get_gpu_clock_constraints(&self) -> zbus::Result<HashMap<String, OwnedValue>>;

    /// The GPU clock range `(min_mhz, max_mhz)` currently committed to
    /// hardware. `(0, 0)` when the device has no programmable range or
    /// the daemon isn't managing GPU clocks (firmware auto).
    fn get_gpu_clock_range(&self) -> zbus::Result<(u32, u32)>;

    /// Active GPU-clock selection: a preset name (`silent`, `balanced`,
    /// `aggressive`), `custom` for an explicit range, or `auto` when the
    /// firmware is in charge.
    #[zbus(property)]
    fn gpu_clock_range(&self) -> zbus::Result<String>;

    /// Whether the daemon is currently inferring the GPU clock ceiling
    /// from the TDP envelope (mirrors `auto_cooling` for the fan curve).
    #[zbus(property)]
    fn gpu_follows_tdp(&self) -> zbus::Result<bool>;
}
