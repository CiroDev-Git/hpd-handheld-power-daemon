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
/// CPU/GPU fan-RPM reader (read-only — fan curves stay in firmware).
pub mod fan;
/// SPL / SPPT / FPPT envelope backend backed by the upstream
/// `asus-armoury` firmware-attributes driver.
pub mod power;
/// ACPI platform-profile reader/writer (`/sys/firmware/acpi/platform_profile`).
pub mod profile;

use hpd_capabilities::backend::HwBackend;
use hpd_capabilities::charge::ChargeControl;
use hpd_capabilities::fan::FanControl;
use hpd_capabilities::platform_profile::PlatformProfile;
use hpd_capabilities::power::PowerEnvelope;
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
    /// ACPI platform-profile backend.
    pub profile: profile::AsusProfileBackend<S>,
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
            profile: profile::AsusProfileBackend::new(sysfs),
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
}
