// SPDX-License-Identifier: GPL-3.0-or-later

//! Extended, best-effort system telemetry (battery, CPU/GPU frequency,
//! GPU load, VRAM).
//!
//! Distinct from [`thermal::ThermalSensors`](crate::thermal::ThermalSensors)
//! (CPU/GPU temperature + SoC power, present on every ASUS handheld) and
//! [`fan::FanControl`](crate::fan::FanControl) (fan RPM): every field here
//! is genuinely optional across devices (a desktop dGPU box has no
//! battery; a board without amdgpu's `gpu_busy_percent` support simply
//! omits that key), so every accessor defaults to `Ok(None)` rather than
//! requiring a vendor backend to stub out sensors it does not have.
//!
//! `Ok(None)` means "this hardware does not expose this reading" — the
//! D-Bus `get_telemetry()` surface omits the key entirely rather than
//! sending a placeholder value a client might mistake for real data.

use crate::units::PowerMilliwatts;
use hpd_error::HpdError;

/// Read-only access to telemetry beyond core temperature/fan/power: battery
/// power draw and health, CPU/GPU clocks, GPU utilisation and VRAM.
///
/// Every method defaults to `Ok(None)` so a vendor backend only overrides
/// what its hardware actually exposes.
pub trait SystemTelemetry: Send + Sync {
    /// Total system power draw from the battery (discharge), in
    /// milliwatts. `None` while charging/full or when unreadable — use
    /// [`Self::get_battery_status`] to tell those apart.
    fn get_battery_power(&self) -> Result<Option<PowerMilliwatts>, HpdError> {
        Ok(None)
    }

    /// Battery charge, `0..=100`.
    fn get_battery_percent(&self) -> Result<Option<u8>, HpdError> {
        Ok(None)
    }

    /// Raw kernel `power_supply` status string (`Charging`,
    /// `Discharging`, `Full`, `Not charging`, `Unknown`), passed through
    /// verbatim rather than re-modelled as an enum — it is display-only.
    fn get_battery_status(&self) -> Result<Option<String>, HpdError> {
        Ok(None)
    }

    /// Battery health: current full-charge capacity as a percentage of
    /// the factory design capacity, `0..=100` (values over 100 — a
    /// freshly calibrated battery — are not clamped; callers display
    /// what the kernel reports).
    fn get_battery_health_pct(&self) -> Result<Option<u8>, HpdError> {
        Ok(None)
    }

    /// Battery charge/discharge cycle count.
    fn get_battery_cycles(&self) -> Result<Option<u32>, HpdError> {
        Ok(None)
    }

    /// Average current CPU core frequency, in MHz.
    fn get_cpu_freq_mhz(&self) -> Result<Option<u32>, HpdError> {
        Ok(None)
    }

    /// Current GPU core frequency, in MHz.
    fn get_gpu_freq_mhz(&self) -> Result<Option<u32>, HpdError> {
        Ok(None)
    }

    /// GPU busy/utilisation percentage, `0..=100`.
    fn get_gpu_busy_pct(&self) -> Result<Option<u8>, HpdError> {
        Ok(None)
    }

    /// VRAM currently in use, in megabytes.
    fn get_vram_used_mb(&self) -> Result<Option<u32>, HpdError> {
        Ok(None)
    }

    /// Total VRAM available, in megabytes.
    fn get_vram_total_mb(&self) -> Result<Option<u32>, HpdError> {
        Ok(None)
    }

    /// Raw GPU throttle-status bitmask, when the kernel exposes one.
    /// No ASUS handheld backend currently populates this (there is no
    /// stable, non-debugfs sysfs attribute for it as of this writing);
    /// the accessor exists so a future kernel/backend can add it without
    /// a trait-breaking change.
    fn get_gpu_throttle_status(&self) -> Result<Option<u64>, HpdError> {
        Ok(None)
    }
}
