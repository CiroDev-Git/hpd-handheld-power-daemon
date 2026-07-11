// SPDX-License-Identifier: GPL-3.0-or-later

//! ASUS armoury firmware-attribute backend (workspace layer **L1**).
//!
//! [`AsusBackend`] is a thin composition of four single-responsibility
//! sub-backends (`power`, `charge`, `fan`, `profile`), each
//! implementing its respective L2 capability trait against
//! [`hpd_sysfs::SysfsIo`]. The aggregate exposes them to the rest of
//! the workspace through [`hpd_capabilities::backend::HwBackend`]'s
//! accessor methods.
//!
//! Every ASUS handheld this crate targets implements all four
//! capabilities, so every accessor here returns `Some(...)`. Vendors
//! with partial hardware support implement fewer accessors — see
//! [`hpd_capabilities::backend::HwBackend`] for the contract.

/// Battery charge-threshold backend backed by
/// `/sys/class/power_supply/BAT0/charge_control_end_threshold`.
pub mod charge;
/// DMI-based detection of the supported ASUS handheld variants.
pub mod detect;
/// CPU/GPU fan-RPM reader (read-only telemetry).
pub mod fan;
/// EC-mediated custom fan-curve writer (`asus_custom_fan_curve` hwmon).
pub mod fan_curve;
/// GPU clock-range writer (amdgpu `pp_od_clk_voltage` OverDrive).
pub mod gpu_clock;
/// hwmon device lookup by stable `name` attribute.
mod hwmon;
/// SPL / SPPT / FPPT envelope backend backed by the upstream
/// `asus-armoury` firmware-attributes driver.
pub mod power;
/// `power_supply` class node lookup by `type` (AC / battery).
mod power_supply;
/// ACPI platform-profile reader/writer (`/sys/firmware/acpi/platform_profile`).
pub mod profile;
/// Extended telemetry reader (battery, CPU/GPU clocks, GPU load, VRAM).
pub mod telemetry;
/// CPU/GPU temperature reader (`k10temp` / `amdgpu` hwmon).
pub mod thermal;

use hpd_capabilities::backend::HwBackend;
use hpd_capabilities::charge::ChargeControl;
use hpd_capabilities::fan::FanControl;
use hpd_capabilities::fan_curve::FanCurveControl;
use hpd_capabilities::gpu_clock::GpuClockRangeControl;
use hpd_capabilities::platform_profile::PlatformProfile;
use hpd_capabilities::power::PowerEnvelope;
use hpd_capabilities::telemetry::SystemTelemetry;
use hpd_capabilities::thermal::ThermalSensors;
use hpd_sysfs::SysfsIo;

/// Composition root for the ASUS backend. Owns the four
/// single-responsibility sub-backends and exposes them via the
/// [`HwBackend`] accessor surface.
pub struct AsusBackend<S: SysfsIo + Clone> {
    /// SPL / SPPT / FPPT envelope backend.
    pub power: power::AsusPowerBackend<S>,
    /// Battery charge-threshold backend.
    pub charge: charge::AsusChargeBackend<S>,
    /// CPU/GPU fan-RPM backend (read-only).
    pub fan: fan::AsusFanBackend<S>,
    /// EC-mediated custom fan-curve backend (write).
    pub fan_curve: fan_curve::AsusFanCurveBackend<S>,
    /// ACPI platform-profile backend.
    pub profile: profile::AsusProfileBackend<S>,
    /// CPU/GPU temperature backend (read-only).
    pub thermal: thermal::AsusThermalBackend<S>,
    /// Extended telemetry backend (battery, CPU/GPU clocks, GPU load,
    /// VRAM — read-only).
    pub telemetry: telemetry::AsusTelemetryBackend<S>,
    /// GPU clock-range backend (amdgpu OverDrive, write).
    pub gpu_clock: gpu_clock::AsusGpuClockBackend<S>,
}

impl<S: SysfsIo + Clone> AsusBackend<S> {
    /// Build a fresh `AsusBackend` from a single `SysfsIo`. The handle
    /// is cloned once per sub-backend; `RealSysfs` is zero-sized so
    /// this is free in production, and `MockSysfs` shares its
    /// `TempDir` via `Arc` so all four sub-backends see the same
    /// in-memory tree.
    pub fn new(sysfs: S) -> Self {
        Self {
            power: power::AsusPowerBackend::new(sysfs.clone()),
            charge: charge::AsusChargeBackend::new(sysfs.clone()),
            fan: fan::AsusFanBackend::new(sysfs.clone()),
            fan_curve: fan_curve::AsusFanCurveBackend::new(sysfs.clone()),
            profile: profile::AsusProfileBackend::new(sysfs.clone()),
            thermal: thermal::AsusThermalBackend::new(sysfs.clone()),
            telemetry: telemetry::AsusTelemetryBackend::new(sysfs.clone()),
            gpu_clock: gpu_clock::AsusGpuClockBackend::new(sysfs),
        }
    }
}

impl<S: SysfsIo + Clone + 'static> HwBackend for AsusBackend<S> {
    fn power(&self) -> &dyn PowerEnvelope {
        &self.power
    }

    fn charge(&self) -> Option<&dyn ChargeControl> {
        Some(&self.charge)
    }

    fn profile(&self) -> Option<&dyn PlatformProfile> {
        Some(&self.profile)
    }

    fn fan(&self) -> Option<&dyn FanControl> {
        Some(&self.fan)
    }

    fn fan_curve(&self) -> Option<&dyn FanCurveControl> {
        Some(&self.fan_curve)
    }

    fn thermal(&self) -> Option<&dyn ThermalSensors> {
        Some(&self.thermal)
    }

    fn telemetry(&self) -> Option<&dyn SystemTelemetry> {
        Some(&self.telemetry)
    }

    fn gpu_clock(&self) -> Option<&dyn GpuClockRangeControl> {
        Some(&self.gpu_clock)
    }
}
