// SPDX-License-Identifier: GPL-3.0-or-later

use zbus::proxy;

#[proxy(
    interface = "dev.cirodev.hpd.PowerDaemon1",
    default_service = "dev.cirodev.hpd.PowerDaemon1",
    default_path = "/dev/cirodev/hpd/PowerDaemon1"
)]
trait PowerDaemon {
    fn is_ac_connected(&self) -> zbus::Result<bool>;
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

    /// Program a named custom fan curve (`silent`, `balanced`,
    /// `aggressive`). Resolved to the model's concrete curve by the
    /// daemon and re-applied across suspend/resume.
    async fn set_fan_curve(&self, preset: &str) -> zbus::Result<()>;

    /// Hand fan control back to the firmware's automatic curve.
    async fn reset_fan_curve(&self) -> zbus::Result<()>;

    /// Active fan-curve selection: a preset name, `custom`, or `auto`.
    #[zbus(property)]
    fn fan_curve(&self) -> zbus::Result<String>;

    /// Live thermal telemetry: `(cpu_temp_c, gpu_temp_c, cpu_fan_rpm,
    /// gpu_fan_rpm)`. Any field is `i32::MIN` when unavailable.
    fn get_thermal_status(&self) -> zbus::Result<(i32, i32, i32, i32)>;
}
