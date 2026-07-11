// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU clock-range control capability.
//!
//! Where [`fan_curve::FanCurveControl`](crate::fan_curve::FanCurveControl)
//! writes an 8-point temperature→duty curve for the EC to run
//! autonomously, this constrains the *frequency* range amdgpu's own DPM
//! (dynamic power management) is allowed to pick from — a floor/ceiling on
//! the GPU core clock (SCLK), not a fixed value. The firmware still
//! manages voltage/DPM-level selection within whatever range is set.
//!
//! ## Scope: SCLK only
//!
//! This trait is deliberately scoped to the GPU core clock range alone.
//! amdgpu's `pp_od_clk_voltage` interface can, on some (mostly discrete)
//! GPUs, also expose a memory clock range (`OD_MCLK`) and a voltage curve
//! (`OD_VDDC_CURVE`) — neither exists on the APUs this workspace targets
//! (no separate VRAM clock domain, no per-point voltage control on this
//! generation, confirmed against a real ROG Xbox Ally X). A future device
//! that exposes those needs a *separate* capability trait, not a
//! silently-widened version of this one.
//!
//! ## Why the constraints are read live, not hardcoded
//!
//! Unlike [`FanCurveConstraints`](crate::fan_curve::FanCurveConstraints)
//! (a per-model calibrated safety floor — Class C data, see
//! `docs/dev/GAMING-ROADMAP-es.md` §0b — with no sysfs source of truth),
//! the safe clock range here is reported directly by the kernel driver via
//! `pp_od_clk_voltage`'s `OD_RANGE` block. That makes it Class A data:
//! portable to any device exposing this same generic amdgpu interface,
//! without a per-model capture.
//!
//! ## Fail-safe framing
//!
//! This generation of amdgpu OverDrive exposes frequency only, never
//! voltage — the worst case of a bad range is the GPU getting stuck
//! non-adaptive (a performance/thermal regression), not the "fatal
//! hardware damage" the kernel's own docs warn about for voltage
//! injection, which this hardware does not expose.

use hpd_error::HpdError;
use serde::{Deserialize, Serialize};

use crate::fan_curve::FanCurvePreset;

/// An explicit GPU core-clock (SCLK) floor/ceiling, in MHz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuClockRange {
    /// Lowest frequency (MHz) the DPM may select.
    pub min_mhz: u32,
    /// Highest frequency (MHz) the DPM may select.
    pub max_mhz: u32,
}

/// This device's supported clock range, read live from the kernel driver
/// (`OD_RANGE`) — see the module docs on why this is Class A, not a
/// hardcoded per-model table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GpuClockConstraints {
    /// Lowest SCLK (MHz) the hardware supports.
    pub range_min_mhz: u32,
    /// Highest SCLK (MHz) the hardware supports.
    pub range_max_mhz: u32,
}

impl GpuClockRange {
    /// Reject a range that is inverted/degenerate or outside the device's
    /// supported bounds — the daemon's own defence-in-depth check on top
    /// of the kernel's own DPM-level validation.
    pub fn validate_against(&self, constraints: &GpuClockConstraints) -> Result<(), HpdError> {
        if self.min_mhz >= self.max_mhz {
            return Err(HpdError::InvariantViolation(format!(
                "gpu clock range min ({} MHz) must be strictly less than max ({} MHz)",
                self.min_mhz, self.max_mhz
            )));
        }
        if self.min_mhz < constraints.range_min_mhz || self.min_mhz > constraints.range_max_mhz {
            return Err(HpdError::InvariantViolation(format!(
                "gpu clock min {} MHz is outside the device's supported range ({}..={} MHz)",
                self.min_mhz, constraints.range_min_mhz, constraints.range_max_mhz
            )));
        }
        if self.max_mhz < constraints.range_min_mhz || self.max_mhz > constraints.range_max_mhz {
            return Err(HpdError::InvariantViolation(format!(
                "gpu clock max {} MHz is outside the device's supported range ({}..={} MHz)",
                self.max_mhz, constraints.range_min_mhz, constraints.range_max_mhz
            )));
        }
        Ok(())
    }
}

/// What the user asked the daemon to apply: either the same tier the
/// auto-inferred fan curve uses (`hpd-core`'s reducer/executor resolve
/// this to a concrete [`GpuClockRange`] via `RuntimeConfig`'s clock
/// fractions before it ever reaches [`GpuClockRangeControl::set_range`] —
/// see that trait's docs on why it never speaks in `GpuClockSelection`
/// itself) or an explicit custom range. Persisted in `ProfileState` so the
/// active selection survives restarts. Wrapped in `Option` there — `None`
/// means firmware auto, the default/steady-state for anyone who never
/// opts in (see [`GpuClockRangeControl::active_range`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GpuClockSelection {
    /// The same tier the auto-inferred fan curve uses for the current
    /// SPL — reused rather than a new enum so one inference call drives
    /// both the fan curve and the GPU clock ceiling.
    Preset(FanCurvePreset),
    /// An explicit range supplied by the caller.
    Custom(GpuClockRange),
}

/// Write access to the GPU's DPM clock range (amdgpu `pp_od_clk_voltage`
/// + `power_dpm_force_performance_level`).
///
/// Deliberately speaks in concrete [`GpuClockRange`], never
/// [`GpuClockSelection`] — resolving a `Preset(tier)` to a concrete range
/// needs `RuntimeConfig`'s clock fractions, which this L1 trait must
/// never see (threading it down from `hpd-core` would be a layering
/// violation). The caller (the executor, which does hold the runtime
/// config) resolves `Preset` to a range before calling [`Self::set_range`];
/// `GpuClockSelection` stays a purely domain-level (`hpd-core`) concept
/// for *why* a range is active, not something this trait speaks in.
pub trait GpuClockRangeControl: Send + Sync {
    /// Program the given range into the hardware and switch the DPM to
    /// manual mode. Implementations must read back the committed range
    /// and fail (`Err`) if the driver did not accept it — and must leave
    /// the device back in firmware `auto` mode on failure rather than
    /// stuck in `manual` with no valid committed range (see the backend's
    /// own docs for the exact write sequence).
    fn set_range(&self, range: &GpuClockRange) -> Result<(), HpdError>;

    /// Return the GPU clock to the firmware's automatic DPM (`auto`).
    fn reset_to_auto(&self) -> Result<(), HpdError>;

    /// The range the driver is **actually** running, derived live from
    /// hardware: `None` when `power_dpm_force_performance_level == "auto"`,
    /// else `Some(range)`. Used by the executor's rollback path so a
    /// silently-failed write never leaves the daemon reporting a state
    /// the driver didn't actually accept.
    fn active_range(&self) -> Result<Option<GpuClockRange>, HpdError>;

    /// This device's live-read [`GpuClockConstraints`]. Fallible — unlike
    /// [`crate::fan_curve::FanCurveControl::constraints`]'s hardcoded
    /// table, this is a real sysfs read that can fail (e.g.
    /// `FeatureUnsupported` if `pp_od_clk_voltage` is absent).
    fn constraints(&self) -> Result<GpuClockConstraints, HpdError>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn sample_constraints() -> GpuClockConstraints {
        GpuClockConstraints {
            range_min_mhz: 600,
            range_max_mhz: 2900,
        }
    }

    #[test]
    fn valid_range_passes() {
        let r = GpuClockRange {
            min_mhz: 600,
            max_mhz: 1600,
        };
        assert!(r.validate_against(&sample_constraints()).is_ok());
    }

    #[test]
    fn range_matching_full_device_bounds_passes() {
        let r = GpuClockRange {
            min_mhz: 600,
            max_mhz: 2900,
        };
        assert!(r.validate_against(&sample_constraints()).is_ok());
    }

    #[test]
    fn inverted_range_is_rejected() {
        let r = GpuClockRange {
            min_mhz: 1600,
            max_mhz: 800,
        };
        assert!(r.validate_against(&sample_constraints()).is_err());
    }

    #[test]
    fn degenerate_equal_range_is_rejected() {
        let r = GpuClockRange {
            min_mhz: 1000,
            max_mhz: 1000,
        };
        assert!(r.validate_against(&sample_constraints()).is_err());
    }

    #[test]
    fn min_below_device_floor_is_rejected() {
        let r = GpuClockRange {
            min_mhz: 200,
            max_mhz: 1600,
        };
        let err = r.validate_against(&sample_constraints()).unwrap_err();
        assert!(err
            .to_string()
            .contains("outside the device's supported range"));
    }

    #[test]
    fn max_above_device_ceiling_is_rejected() {
        let r = GpuClockRange {
            min_mhz: 600,
            max_mhz: 3200,
        };
        assert!(r.validate_against(&sample_constraints()).is_err());
    }
}
