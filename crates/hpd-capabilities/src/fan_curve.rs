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

    /// Reject a **user-supplied** custom curve against a model's
    /// [`FanCurveConstraints`] — stricter than [`Self::validate`], which
    /// only guards the compile-time preset tables (already known-good)
    /// against accidental non-monotonicity. A hand-drawn curve gets:
    ///
    /// - Strictly increasing temperatures (no two points at the same
    ///   temperature — unlike a preset, there is no "parked tail" case to
    ///   allow for a curve someone just drew).
    /// - Non-decreasing duty (a curve may still plateau).
    /// - Every point within `[temp_min_c, temp_max_c]` × `[pwm_min, pwm_max]`.
    /// - Every point at or above a `safety_floor` threshold meets that
    ///   threshold's minimum duty — defense in depth on top of the EC's
    ///   own firmware failsafes; see [`FanCurveConstraints`].
    ///
    /// Errors name the offending point (1-based, matching the hwmon
    /// `auto_pointN` numbering) and the specific rule it broke.
    pub fn validate_against(&self, constraints: &FanCurveConstraints) -> Result<(), HpdError> {
        for window in self.points.windows(2) {
            let (prev, next) = (window[0], window[1]);
            if next.temp_c <= prev.temp_c {
                return Err(HpdError::InvariantViolation(format!(
                    "fan curve temperatures must strictly increase: {}°C after {}°C",
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

        for (i, p) in self.points.iter().enumerate() {
            let point = i + 1;
            if p.temp_c < constraints.temp_min_c || p.temp_c > constraints.temp_max_c {
                return Err(HpdError::InvariantViolation(format!(
                    "point {point}: {}°C is outside the device's supported range ({}..={}°C)",
                    p.temp_c, constraints.temp_min_c, constraints.temp_max_c
                )));
            }
            if p.pwm < constraints.pwm_min || p.pwm > constraints.pwm_max {
                return Err(HpdError::InvariantViolation(format!(
                    "point {point}: pwm {} is outside the device's supported range ({}..={})",
                    p.pwm, constraints.pwm_min, constraints.pwm_max
                )));
            }
            // The strictest applicable floor is the highest min_pwm among
            // every threshold this point's temperature has reached.
            let required = constraints
                .safety_floor
                .iter()
                .filter(|(threshold, _)| p.temp_c >= *threshold)
                .map(|(_, min_pwm)| *min_pwm)
                .max();
            if let Some(required) = required {
                if p.pwm < required {
                    return Err(HpdError::InvariantViolation(format!(
                        "point {point}: {}°C requires pwm ≥ {required} (safety floor), got {}",
                        p.temp_c, p.pwm
                    )));
                }
            }
        }

        Ok(())
    }
}

/// Model-specific limits and safety floor for a custom fan curve —
/// returned by [`FanCurveControl::constraints`] so a client (the plugin's
/// curve editor, `hpdctl cool set-custom`) can validate precisely for the
/// running device rather than against a hardcoded guess, and so
/// [`FanCurve::validate_against`] can enforce the same rules server-side.
///
/// `safety_floor` is an unordered list of `(temp_threshold_c, min_pwm)`
/// pairs: at or above `temp_threshold_c`, `pwm` must be at least
/// `min_pwm`. It is defense in depth on top of the EC's own firmware
/// failsafes — hpd refuses to even *ask* the EC for a reckless curve.
/// Lives as a per-model constant next to the calibrated presets (see the
/// vendor backend); a new device with no on-hardware capture yet should
/// inherit the most conservative floor in its family rather than none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanCurveConstraints {
    /// Lowest temperature (°C) the device accepts in a curve point.
    pub temp_min_c: u8,
    /// Highest temperature (°C) the device accepts in a curve point.
    pub temp_max_c: u8,
    /// Lowest duty cycle the device accepts.
    pub pwm_min: u8,
    /// Highest duty cycle the device accepts (typically [`PWM_MAX`]).
    pub pwm_max: u8,
    /// `(temp_threshold_c, min_pwm)` pairs; see the struct docs.
    pub safety_floor: Vec<(u8, u8)>,
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

    /// The selection the EC is **actually** running, derived live from
    /// hardware: `None` when the firmware's automatic curve is active,
    /// `Some(Preset)` when the stored points match a known preset, else
    /// `Some(Custom)`. Used by the executor's rollback path so a
    /// silently-rejected write never leaves the daemon reporting a level
    /// the EC didn't accept (the reported level always reflects reality).
    fn active_selection(&self) -> Result<Option<FanCurveSelection>, HpdError>;

    /// This model's [`FanCurveConstraints`] — the range and safety floor
    /// a custom curve must respect. Mandatory (not defaulted): any
    /// backend implementing this trait writes real hardware and so must
    /// state what that hardware will and won't tolerate.
    fn constraints(&self) -> FanCurveConstraints;
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

    fn sample_constraints() -> FanCurveConstraints {
        FanCurveConstraints {
            temp_min_c: 30,
            temp_max_c: 95,
            pwm_min: 0,
            pwm_max: PWM_MAX,
            safety_floor: vec![(85, 150), (90, 200)],
        }
    }

    fn valid_custom_curve() -> FanCurve {
        FanCurve::new([
            FanCurvePoint::new(45, 20),
            FanCurvePoint::new(54, 50),
            FanCurvePoint::new(62, 95),
            FanCurvePoint::new(69, 145),
            FanCurvePoint::new(75, 190),
            FanCurvePoint::new(80, 225),
            FanCurvePoint::new(85, 255),
            FanCurvePoint::new(92, 255),
        ])
    }

    #[test]
    fn valid_custom_curve_passes_validate_against() {
        assert!(valid_custom_curve()
            .validate_against(&sample_constraints())
            .is_ok());
    }

    #[test]
    fn equal_temperatures_are_rejected_by_validate_against() {
        // Unlike `validate()`, a repeated temperature is not allowed in a
        // user-supplied curve.
        let mut c = valid_custom_curve();
        c.points[3] = FanCurvePoint::new(c.points[2].temp_c, 120);
        assert!(c.validate_against(&sample_constraints()).is_err());
    }

    #[test]
    fn decreasing_duty_is_rejected_by_validate_against() {
        let mut c = valid_custom_curve();
        c.points[5] = FanCurvePoint::new(c.points[5].temp_c, 10);
        assert!(c.validate_against(&sample_constraints()).is_err());
    }

    #[test]
    fn temperature_outside_device_range_is_rejected() {
        let mut c = valid_custom_curve();
        c.points[7] = FanCurvePoint::new(96, 255); // > temp_max_c
        assert!(c.validate_against(&sample_constraints()).is_err());
    }

    #[test]
    fn pwm_outside_device_range_is_rejected() {
        // Range check only (constraints below would also flag the floor);
        // use a low temperature so only the pwm range check can fire.
        let mut c = valid_custom_curve();
        c.points[0] = FanCurvePoint::new(31, 0);
        let mut constraints = sample_constraints();
        constraints.pwm_min = 5;
        assert!(c.validate_against(&constraints).is_err());
    }

    /// Build a curve sharing the same low/mid ramp (well clear of any
    /// safety floor) but with caller-chosen last two points, so a test
    /// can target the 85 °C / 90 °C floor thresholds without accidentally
    /// tripping the non-decreasing-duty check on an unrelated point.
    fn curve_with_last_two(second_last: (u8, u8), last: (u8, u8)) -> FanCurve {
        FanCurve::new([
            FanCurvePoint::new(45, 20),
            FanCurvePoint::new(54, 40),
            FanCurvePoint::new(62, 60),
            FanCurvePoint::new(69, 80),
            FanCurvePoint::new(75, 100),
            FanCurvePoint::new(80, 120),
            FanCurvePoint::new(second_last.0, second_last.1),
            FanCurvePoint::new(last.0, last.1),
        ])
    }

    #[test]
    fn safety_floor_violation_is_rejected_with_specific_message() {
        // 90°C requires pwm >= 200; give it less.
        let c = curve_with_last_two((85, 150), (90, 199));
        let err = c.validate_against(&sample_constraints()).unwrap_err();
        assert!(
            err.to_string().contains("90°C requires pwm ≥ 200"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn safety_floor_satisfied_exactly_at_the_boundary_passes() {
        let c = curve_with_last_two((85, 150), (90, 200));
        assert!(c.validate_against(&sample_constraints()).is_ok());
    }

    #[test]
    fn stricter_higher_threshold_floor_applies_even_if_the_lower_one_is_also_met() {
        // A point at 92°C must satisfy the 90°C floor (>=200), not just
        // the weaker 85°C floor (>=150) — pwm stays non-decreasing
        // (150 -> 180) so this is purely a floor violation, not a
        // monotonicity one.
        let c = curve_with_last_two((85, 150), (92, 180));
        let err = c.validate_against(&sample_constraints()).unwrap_err();
        assert!(
            err.to_string().contains("requires pwm ≥ 200"),
            "unexpected message: {err}"
        );
    }
}
