// SPDX-License-Identifier: GPL-3.0-or-later

use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::ProfileName;

/// Represents an action with side effect (I/O) that Executor should dispatch.
///
/// D-Bus `PropertiesChanged` signals are NOT modelled here: they are emitted
/// implicitly by a dedicated task in `hpd-daemon` that subscribes to the
/// state `watch::Receiver` and calls the zbus-generated `<prop>_changed`
/// notifiers. See `daemon::main::spawn_properties_changed_emitter`.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Write a new power envelope to the L1 backend.
    ApplyPowerEnvelope(PowerEnvelopeTarget),
    /// Write the ACPI platform profile to the L1 backend.
    ApplyPlatformProfile(ProfileName),
    /// Write the battery charge end threshold to the L1 backend.
    ApplyChargeThreshold(u8),
    /// Program a custom fan curve into the EC via the L1 backend. The
    /// backend resolves a `Preset` to its model's concrete curves.
    ApplyFanCurve(FanCurveSelection),
    /// Hand fan control back to the firmware's automatic curve.
    ResetFanCurve,
    /// Program a GPU clock range into the hardware via the L1 backend.
    /// Carries the curated tier (never a resolved MHz value, and never an
    /// arbitrary caller-supplied range — there is no such thing anymore,
    /// see `GpuClockSelection`'s docs) because resolving it to a concrete
    /// range needs BOTH `RuntimeConfig`'s clock fractions (available to
    /// the pure reducer) AND the live `GpuClockConstraints` (a hardware
    /// read the reducer must never perform) — so the Executor is what
    /// resolves it to a concrete `GpuClockRange` immediately before
    /// calling `GpuClockRangeControl::set_range`.
    ApplyGpuClockRange(FanCurvePreset),
    /// Hand the GPU clock back to firmware auto.
    ResetGpuClocks,
    /// Flush the current `ProfileState` to disk via the persister.
    PersistState,
}
