// SPDX-License-Identifier: GPL-3.0-or-later

//! Fan-curve control capability.
//!
//! Where [`fan::FanControl`](crate::fan::FanControl) only *reads* RPM,
//! this capability *writes* a temperature→duty-cycle curve that the
//! embedded controller (EC) then runs autonomously. ASUS handhelds
//! expose an 8-point curve per fan through the `asus_custom_fan_curve`
//! hwmon device (`pwmN_auto_pointK_{temp,pwm}`).
//!
//! ## Why this is fail-safe
//!
//! We never drive raw PWM. We hand the EC a set of *auto-points* and
//! the EC closes the control loop in firmware. If `hpd` crashes or is
//! killed mid-session the fans keep following the last curve we wrote —
//! they do not freeze at a fixed duty or stop. [`FanCurveControl::reset_to_auto`]
//! returns control to the firmware's built-in curve.
//!
//! ## Layering
//!
//! This L2 trait and its value types are hardware-agnostic. The
//! concrete per-model preset *values* live in the L1 backend
//! (`hpd-backend-asus`), which is the single source of truth for what
//! `Silent`/`Balanced`/`Aggressive` mean on a given console.

use std::str::FromStr;

use hpd_error::HpdError;
use serde::{Deserialize, Serialize};

/// Number of `(temperature, pwm)` points in a fan curve. Fixed at 8 to
/// match the ASUS `asus_custom_fan_curve` hwmon contract
/// (`pwmN_auto_point1..8`).
pub const FAN_CURVE_POINTS: usize = 8;

/// Maximum PWM duty value the hardware accepts (full speed). The kernel
/// expresses fan-curve duty as a 0–255 byte.
pub const PWM_MAX: u8 = 255;

/// A single point on a fan curve: at `temp_c` degrees Celsius the fan
/// runs at `pwm` duty (0–[`PWM_MAX`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanCurvePoint {
    /// Temperature threshold in degrees Celsius.
    pub temp_c: u8,
    /// Fan duty cycle, `0..=255`, where 255 is full speed.
    pub pwm: u8,
}

impl FanCurvePoint {
    /// Construct a point. `const` so preset tables can be built at
    /// compile time.
    pub const fn new(temp_c: u8, pwm: u8) -> Self {
        Self { temp_c, pwm }
    }
}

/// An 8-point fan curve. The points must be ordered by non-decreasing
/// temperature *and* non-decreasing duty — a sane curve never spins the
/// fan slower as the chip gets hotter. Construct freely; call
/// [`FanCurve::validate`] at trust boundaries (D-Bus / config input)
/// before handing a curve to the hardware. Compile-time preset
/// constants are known-good and skip the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FanCurve {
    /// The eight ordered curve points.
    pub points: [FanCurvePoint; FAN_CURVE_POINTS],
}

impl FanCurve {
    /// Build a curve from its points without validating. Intended for
    /// the compile-time preset tables in the backend, which are
    /// known-good by construction.
    pub const fn new(points: [FanCurvePoint; FAN_CURVE_POINTS]) -> Self {
        Self { points }
    }

    /// Reject curves that are not monotonic. Both temperature and duty
    /// must be non-decreasing across the eight points. Used at input
    /// boundaries — see the crate-level note on validation.
    pub fn validate(&self) -> Result<(), HpdError> {
        for window in self.points.windows(2) {
            let (prev, next) = (window[0], window[1]);
            if next.temp_c < prev.temp_c {
                return Err(HpdError::InvariantViolation(format!(
                    "fan curve temperatures must not decrease: {}°C after {}°C",
                    next.temp_c, prev.temp_c
                )));
            }
            if next.pwm < prev.pwm {
                return Err(HpdError::InvariantViolation(format!(
                    "fan curve duty must not decrease: pwm {} after {}",
                    next.pwm, prev.pwm
                )));
            }
        }
        Ok(())
    }
}

/// A named, model-defined fan curve. The concrete temperature/duty
/// values for each variant live in the vendor backend (see
/// `hpd-backend-asus`) because they are per-console tuning, not a
/// hardware-agnostic constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FanCurvePreset {
    /// Quietest curve that still extends control into the high-temp
    /// range the firmware default leaves undefined.
    Silent,
    /// Default after install: noticeably cooler than firmware, still
    /// reasonably quiet.
    Balanced,
    /// Prioritises cooling (screen/back temperature) over noise,
    /// Armoury-Crate "Turbo" style.
    Aggressive,
}

impl FanCurvePreset {
    /// Lowercase identifier used on the CLI, in config, and over D-Bus.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Balanced => "balanced",
            Self::Aggressive => "aggressive",
        }
    }

    /// Every preset, for help text and validation.
    pub const ALL: [FanCurvePreset; 3] = [Self::Silent, Self::Balanced, Self::Aggressive];
}

impl FromStr for FanCurvePreset {
    type Err = HpdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "silent" => Ok(Self::Silent),
            "balanced" => Ok(Self::Balanced),
            "aggressive" => Ok(Self::Aggressive),
            other => Err(HpdError::InvariantViolation(format!(
                "unknown fan curve preset '{}' (expected silent|balanced|aggressive)",
                other
            ))),
        }
    }
}

/// What the user asked the daemon to apply: either a named preset (the
/// backend resolves it to its model's concrete curves) or an explicit
/// pair of CPU/GPU curves. Persisted in `ProfileState` so the active
/// selection survives restarts and can be re-applied on resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FanCurveSelection {
    /// A model-defined named preset.
    Preset(FanCurvePreset),
    /// Explicit per-fan curves supplied by the caller.
    Custom {
        /// Curve for the CPU/SoC fan (`pwm1`).
        cpu: FanCurve,
        /// Curve for the GPU fan (`pwm2`).
        gpu: FanCurve,
    },
}

/// The concrete curves currently programmed into the hardware, as read
/// back from the EC. Returned by [`FanCurveControl::get_curves`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActiveFanCurves {
    /// CPU/SoC fan curve (`pwm1`).
    pub cpu: FanCurve,
    /// GPU fan curve (`pwm2`).
    pub gpu: FanCurve,
}

/// Write access to the EC-mediated custom fan curve.
///
/// Implementations resolve a [`FanCurveSelection::Preset`] to their
/// model's concrete curves internally, so the daemon's reducer and
/// executor stay hardware-agnostic.
pub trait FanCurveControl: Send + Sync {
    /// Program the given selection into the hardware and switch the EC
    /// to custom-curve mode. Implementations should read back the
    /// written points and fail (`Err`) if the EC did not accept them.
    fn apply(&self, selection: &FanCurveSelection) -> Result<(), HpdError>;

    /// Return fan control to the firmware's built-in automatic curve.
    fn reset_to_auto(&self) -> Result<(), HpdError>;

    /// Read back the curves currently programmed into the EC.
    fn get_curves(&self) -> Result<ActiveFanCurves, HpdError>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn flat(temp: u8, pwm: u8) -> FanCurve {
        FanCurve::new([FanCurvePoint::new(temp, pwm); FAN_CURVE_POINTS])
    }

    #[test]
    fn monotonic_curve_validates() {
        let c = FanCurve::new([
            FanCurvePoint::new(45, 15),
            FanCurvePoint::new(54, 33),
            FanCurvePoint::new(62, 64),
            FanCurvePoint::new(69, 102),
            FanCurvePoint::new(76, 140),
            FanCurvePoint::new(82, 178),
            FanCurvePoint::new(87, 216),
            FanCurvePoint::new(92, 255),
        ]);
        assert!(c.validate().is_ok());
    }

    #[test]
    fn equal_points_are_allowed() {
        // The firmware default parks its tail on a repeated point.
        assert!(flat(62, 56).validate().is_ok());
    }

    #[test]
    fn decreasing_temperature_is_rejected() {
        let mut c = flat(60, 80);
        c.points[3] = FanCurvePoint::new(40, 80);
        assert!(c.validate().is_err());
    }

    #[test]
    fn decreasing_duty_is_rejected() {
        let mut c = flat(60, 80);
        c.points[5] = FanCurvePoint::new(60, 10);
        assert!(c.validate().is_err());
    }

    #[test]
    fn preset_string_round_trips() {
        for p in FanCurvePreset::ALL {
            assert_eq!(FanCurvePreset::from_str(p.as_str()).unwrap(), p);
        }
        assert_eq!(
            FanCurvePreset::from_str("AGGRESSIVE").unwrap(),
            FanCurvePreset::Aggressive
        );
        assert!(FanCurvePreset::from_str("turbo").is_err());
    }
}
