// SPDX-License-Identifier: GPL-3.0-or-later

//! Platform cooling profile + TDP preset value types and the
//! runtime-tunable [`RuntimeConfig`] that bundles them with the smart-mode
//! boost factors.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Domain representation of a platform cooling profile.
///
/// Conversions to/from strings are designed to be symmetric:
///
/// ```text
/// ProfileName::PowerSaver  <-> "power-saver"
/// ProfileName::Balanced    <-> "balanced"
/// ProfileName::Performance <-> "performance"
/// ProfileName::Custom(s)   <-> s
/// ```
///
/// `FromStr` is case-insensitive and additionally accepts the ACPI-native
/// aliases (`quiet`, `low-power`) as `PowerSaver`. Any unknown value
/// is preserved as `Custom`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileName {
    /// Low fan curve, quiet operation (ACPI "low-power" / "quiet").
    PowerSaver,
    /// Default ACPI "balanced" profile.
    Balanced,
    /// High fan curve for sustained boost workloads.
    Performance,
    /// Catch-all for vendor-specific profiles the kernel exposes but
    /// the daemon does not model explicitly.
    Custom(String),
}

impl fmt::Display for ProfileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProfileName::PowerSaver => f.write_str("power-saver"),
            ProfileName::Balanced => f.write_str("balanced"),
            ProfileName::Performance => f.write_str("performance"),
            ProfileName::Custom(s) => f.write_str(s),
        }
    }
}

impl FromStr for ProfileName {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.trim().is_empty() {
            return Err("profile name cannot be empty".to_string());
        }
        match s.to_lowercase().as_str() {
            "powersaver" | "power-saver" | "quiet" | "low-power" => Ok(ProfileName::PowerSaver),
            "balanced" => Ok(ProfileName::Balanced),
            "performance" => Ok(ProfileName::Performance),
            other => Ok(ProfileName::Custom(other.to_string())),
        }
    }
}

/// Cut-off SPL fractions used by the reducer's auto-profile inference:
/// SPL below `low_frac` → `PowerSaver`, between → `Balanced`, above
/// `high_frac` → `Performance`. Both fields are in `[0.0, 1.0]`.
///
/// `#[serde(default)]` at struct level so a partial TOML (e.g.
/// `low_frac` set without `high_frac`) falls back to the missing
/// field's default rather than erroring. Combined with the
/// `#[serde(flatten)]` carrier in `hpd_daemon::config::DaemonConfig`
/// this is what makes the on-disk config "any subset is valid".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfileThresholds {
    /// Fraction of the SPL range below which the auto-inferred profile is
    /// `PowerSaver`.
    pub low_frac: f32,
    /// Fraction of the SPL range above which the auto-inferred profile is
    /// `Performance`.
    pub high_frac: f32,
}

impl ProfileThresholds {
    /// Production default used by the daemon and most tests: SPL fractions
    /// below 33% map to PowerSaver, 33–67% to Balanced, 67%+ to Performance.
    pub const DEFAULT: Self = Self {
        low_frac: 0.33,
        high_frac: 0.67,
    };

    /// Repair operator-supplied values that would otherwise silently break
    /// the reducer's auto-cooling curve inference (`hpd-core`'s
    /// `infer_fan_curve_from_spl`): each fraction is clamped into
    /// `[0.0, 1.0]`, and if the result still has `low_frac > high_frac` (or
    /// either input was NaN/infinite) the whole pair falls back to
    /// [`Self::DEFAULT`] rather than leaving an inverted range that would
    /// make the tier lookup pick the wrong preset. Never errors — a config
    /// typo must never stop the daemon from starting.
    pub fn sanitized(self) -> Self {
        if !self.low_frac.is_finite() || !self.high_frac.is_finite() {
            return Self::DEFAULT;
        }
        let low_frac = self.low_frac.clamp(0.0, 1.0);
        let high_frac = self.high_frac.clamp(0.0, 1.0);
        if low_frac > high_frac {
            return Self::DEFAULT;
        }
        Self {
            low_frac,
            high_frac,
        }
    }
}

impl Default for ProfileThresholds {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// GPU clock ceiling as a **fraction of the device's live `OD_RANGE`**
/// (never an absolute MHz value — see
/// `hpd_capabilities::gpu_clock`'s module docs on why the safe bounds are
/// read live rather than hardcoded), keyed by the same
/// [`crate::fan_curve::FanCurvePreset`] tier the auto-inferred fan curve
/// already resolves to for the current SPL. `min_mhz` always stays the
/// device's own reported floor — lowering it further isn't possible, and
/// raising it for `Silent` has no benefit.
///
/// **These defaults are untested placeholders**, unlike
/// [`ProfileThresholds::DEFAULT`] (which mirrors historic in-reducer
/// constants already exercised in practice) — real on-device calibration
/// (SCLK/FPS/thermal capture) is a pending manual QA step, not something
/// this type closes out. `#[serde(default)]` at struct level, same
/// partial-TOML tolerance as [`ProfileThresholds`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuClockFractions {
    /// Ceiling fraction of `OD_RANGE` applied when the fan curve tier is
    /// `Silent`.
    pub silent_max_frac: f32,
    /// Ceiling fraction of `OD_RANGE` applied when the fan curve tier is
    /// `Balanced`.
    pub balanced_max_frac: f32,
    /// Ceiling fraction of `OD_RANGE` applied when the fan curve tier is
    /// `Aggressive`. Defaults to `1.0` — a no-op vs. the device's own
    /// ceiling, kept for symmetry/future headroom rather than because it
    /// constrains anything today.
    pub aggressive_max_frac: f32,
}

impl GpuClockFractions {
    /// Untested placeholder defaults — see the struct docs.
    pub const DEFAULT: Self = Self {
        silent_max_frac: 0.55,
        balanced_max_frac: 0.80,
        aggressive_max_frac: 1.0,
    };

    /// Repair operator-supplied values: each fraction clamped into
    /// `[0.0, 1.0]`; if any input is non-finite, or the result isn't
    /// monotonic (`silent ≤ balanced ≤ aggressive`), the whole triple
    /// falls back to [`Self::DEFAULT`] rather than leaving an inverted
    /// ordering that would make a lower tier request a *higher* ceiling
    /// than a more aggressive one. Never errors.
    pub fn sanitized(self) -> Self {
        if !self.silent_max_frac.is_finite()
            || !self.balanced_max_frac.is_finite()
            || !self.aggressive_max_frac.is_finite()
        {
            return Self::DEFAULT;
        }
        let silent_max_frac = self.silent_max_frac.clamp(0.0, 1.0);
        let balanced_max_frac = self.balanced_max_frac.clamp(0.0, 1.0);
        let aggressive_max_frac = self.aggressive_max_frac.clamp(0.0, 1.0);
        if silent_max_frac > balanced_max_frac || balanced_max_frac > aggressive_max_frac {
            return Self::DEFAULT;
        }
        Self {
            silent_max_frac,
            balanced_max_frac,
            aggressive_max_frac,
        }
    }
}

impl Default for GpuClockFractions {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Runtime-tunable subset of the daemon's configuration — everything the
/// reducer + executor consume on every transition. Held by the `Executor`
/// and replaced wholesale when a `Transition::ConfigReload(RuntimeConfig)`
/// arrives, so values like the SPPT/FPPT multipliers can be tuned without
/// a daemon restart.
///
/// Defined in `hpd-capabilities` rather than `hpd-core` so the
/// `Transition` enum can carry it without `hpd-core` needing to know
/// about TOML or the daemon's on-disk schema.
///
/// `#[serde(default)]` at struct level so this type composes safely
/// inside a `#[serde(flatten)]` carrier (see
/// `hpd_daemon::config::DaemonConfig`): a TOML that sets only some of
/// the runtime fields falls back to defaults for the rest instead of
/// erroring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeConfig {
    /// Cooling-profile inference cut-offs (see [`ProfileThresholds`]).
    pub profile_thresholds: ProfileThresholds,
    /// SPL→SPPT multiplier applied by smart-mode `Transition::SetSpl`.
    /// Result is then clamped to `device_limits.sppt_max`.
    pub sppt_factor: f32,
    /// SPL→FPPT multiplier applied by smart-mode `Transition::SetSpl`.
    /// Result is then clamped to `device_limits.fppt_max`.
    pub fppt_factor: f32,
    /// GPU clock ceiling fractions consumed when `gpu_follows_tdp` is on
    /// (see [`GpuClockFractions`]).
    pub gpu_clock_fractions: GpuClockFractions,
}

impl RuntimeConfig {
    /// Defaults match the historic in-reducer constants: 1.15/1.25 boost
    /// multipliers, 0.33/0.67 fan-curve inference cut-offs, plus the
    /// (untested-placeholder) GPU clock fractions.
    pub const DEFAULT: Self = Self {
        profile_thresholds: ProfileThresholds::DEFAULT,
        sppt_factor: 1.15,
        fppt_factor: 1.25,
        gpu_clock_fractions: GpuClockFractions::DEFAULT,
    };

    /// Repair operator-supplied values from `config.toml` that would
    /// otherwise silently misbehave. `sppt_factor`/`fppt_factor` below `1.0`
    /// (or non-finite) fall back to [`Self::DEFAULT`]'s multiplier: a
    /// factor `< 1.0` asks for SPPT/FPPT *below* SPL, which the reducer's
    /// `derive_boosted_envelope` already floors defensively at apply time,
    /// but a config that requests it is almost certainly a typo (e.g.
    /// `1.15` fat-fingered as `.115`) rather than intent, so we replace it
    /// outright instead of silently relying on the floor everywhere.
    /// `profile_thresholds` is delegated to
    /// [`ProfileThresholds::sanitized`]. Never errors — a config typo must
    /// never stop the daemon from starting.
    pub fn sanitized(self) -> Self {
        let valid_factor = |f: f32| f.is_finite() && f >= 1.0;
        Self {
            profile_thresholds: self.profile_thresholds.sanitized(),
            sppt_factor: if valid_factor(self.sppt_factor) {
                self.sppt_factor
            } else {
                Self::DEFAULT.sppt_factor
            },
            fppt_factor: if valid_factor(self.fppt_factor) {
                self.fppt_factor
            } else {
                Self::DEFAULT.fppt_factor
            },
            gpu_clock_fractions: self.gpu_clock_fractions.sanitized(),
        }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Convenience preset for the TDP envelope.
///
/// `TdpPreset` selects a target wattage on the SPL rail; it is **not** the
/// same as [`ProfileName`], which selects the ACPI platform profile (a
/// decoupled power lever). Both can be active independently. When
/// auto-cooling is on, a TDP preset also moves the **fan curve** (never
/// the platform profile, which stays pinned to its configured default):
///
/// | `TdpPreset` | Resulting SPL              | Inferred fan curve (auto-cooling) |
/// |-------------|----------------------------|-----------------------------------|
/// | `Eco`       | `spl_min`                  | `Silent`                          |
/// | `Balanced`  | midpoint of min/max        | `Balanced`                        |
/// | `Max`       | `spl_max`                  | `Aggressive`                      |
///
/// Note the deliberate **non-overlap with `ProfileName`** in naming
/// (e.g. there is no `TdpPreset::Performance`) to avoid the previous
/// confusion where `SystemPreset::Performance` actually meant "midpoint
/// TDP" while `ProfileName::Performance` meant "max cooling".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TdpPreset {
    /// Minimum supported SPL on this hardware.
    Eco,
    /// Midpoint SPL between `spl_min` and `spl_max`.
    Balanced,
    /// Maximum supported SPL on this hardware.
    Max,
}

impl fmt::Display for TdpPreset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TdpPreset::Eco => f.write_str("eco"),
            TdpPreset::Balanced => f.write_str("balanced"),
            TdpPreset::Max => f.write_str("max"),
        }
    }
}

impl FromStr for TdpPreset {
    type Err = String;

    /// Accepts only `eco`, `balanced`, `max` (case-insensitive). The
    /// pre-0.2 names `silent`, `performance`, `turbo` are intentionally
    /// rejected — the same string used to map to different semantics
    /// across `TdpPreset` and `ProfileName`, and accepting aliases would
    /// reintroduce the confusion this enum was renamed to remove.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "eco" => Ok(TdpPreset::Eco),
            "balanced" => Ok(TdpPreset::Balanced),
            "max" => Ok(TdpPreset::Max),
            other => Err(format!(
                "unknown TDP preset '{}': use one of eco, balanced, max",
                other
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    // Test code may use `.unwrap()` / `.expect()` / `panic!` freely;
    // the strict bar in `[workspace.lints.clippy]` applies to
    // production code only.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn profile_display_format_is_kebab_lowercase() {
        assert_eq!(ProfileName::PowerSaver.to_string(), "power-saver");
        assert_eq!(ProfileName::Balanced.to_string(), "balanced");
        assert_eq!(ProfileName::Performance.to_string(), "performance");
        assert_eq!(ProfileName::Custom("foo".into()).to_string(), "foo");
    }

    #[test]
    fn profile_roundtrip_display_to_fromstr() {
        // Display -> FromStr is the identity contract that D-Bus clients rely on.
        let cases = [
            ProfileName::PowerSaver,
            ProfileName::Balanced,
            ProfileName::Performance,
            ProfileName::Custom("very-eco".into()),
        ];
        for p in cases {
            let s = p.to_string();
            let parsed = s
                .parse::<ProfileName>()
                .unwrap_or_else(|e| panic!("roundtrip failed for {:?}: {}", p, e));
            assert_eq!(parsed, p, "Display/FromStr roundtrip broken for {:?}", p);
        }
    }

    #[test]
    fn profile_fromstr_accepts_acpi_aliases_and_case() {
        assert_eq!(
            "quiet".parse::<ProfileName>().unwrap(),
            ProfileName::PowerSaver
        );
        assert_eq!(
            "low-power".parse::<ProfileName>().unwrap(),
            ProfileName::PowerSaver
        );
        assert_eq!(
            "POWER-SAVER".parse::<ProfileName>().unwrap(),
            ProfileName::PowerSaver
        );
        assert_eq!(
            "Balanced".parse::<ProfileName>().unwrap(),
            ProfileName::Balanced
        );
        assert_eq!(
            "PERFORMANCE".parse::<ProfileName>().unwrap(),
            ProfileName::Performance
        );
    }

    #[test]
    fn profile_fromstr_unknown_becomes_custom() {
        assert_eq!(
            "ultra".parse::<ProfileName>().unwrap(),
            ProfileName::Custom("ultra".into())
        );
    }

    #[test]
    fn profile_fromstr_empty_is_rejected() {
        assert!("".parse::<ProfileName>().is_err());
        assert!("   ".parse::<ProfileName>().is_err());
    }

    #[test]
    fn tdp_preset_display_is_lowercase() {
        assert_eq!(TdpPreset::Eco.to_string(), "eco");
        assert_eq!(TdpPreset::Balanced.to_string(), "balanced");
        assert_eq!(TdpPreset::Max.to_string(), "max");
    }

    #[test]
    fn tdp_preset_roundtrip_display_to_fromstr() {
        for p in [TdpPreset::Eco, TdpPreset::Balanced, TdpPreset::Max] {
            assert_eq!(p.to_string().parse::<TdpPreset>().unwrap(), p);
        }
    }

    #[test]
    fn tdp_preset_fromstr_accepts_case_insensitive() {
        assert_eq!("ECO".parse::<TdpPreset>().unwrap(), TdpPreset::Eco);
        assert_eq!(
            "Balanced".parse::<TdpPreset>().unwrap(),
            TdpPreset::Balanced
        );
        assert_eq!("MAX".parse::<TdpPreset>().unwrap(), TdpPreset::Max);
    }

    #[test]
    fn tdp_preset_fromstr_rejects_legacy_aliases() {
        // Deliberate breaking change: pre-0.2 names map to different
        // semantics than the new ones, so we don't accept them as aliases.
        for legacy in ["silent", "performance", "turbo", "Performance", "Turbo"] {
            let err = legacy.parse::<TdpPreset>().unwrap_err();
            assert!(
                err.contains("eco, balanced, max"),
                "error for '{}' should suggest the new names, got: {}",
                legacy,
                err
            );
        }
    }

    // ---------- sanitized() — Audit §2.3 (2026-07) ----------

    #[test]
    fn thresholds_sanitized_passes_through_valid_values() {
        let t = ProfileThresholds {
            low_frac: 0.25,
            high_frac: 0.75,
        };
        assert_eq!(t.clone().sanitized(), t);
    }

    #[test]
    fn thresholds_sanitized_clamps_out_of_range_fractions() {
        let t = ProfileThresholds {
            low_frac: -0.5,
            high_frac: 1.5,
        };
        assert_eq!(
            t.sanitized(),
            ProfileThresholds {
                low_frac: 0.0,
                high_frac: 1.0,
            }
        );
    }

    #[test]
    fn thresholds_sanitized_falls_back_to_default_when_inverted() {
        let t = ProfileThresholds {
            low_frac: 0.9,
            high_frac: 0.1,
        };
        assert_eq!(t.sanitized(), ProfileThresholds::DEFAULT);
    }

    #[test]
    fn thresholds_sanitized_falls_back_to_default_on_non_finite() {
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let t = ProfileThresholds {
                low_frac: bad,
                high_frac: 0.67,
            };
            assert_eq!(t.sanitized(), ProfileThresholds::DEFAULT);
        }
    }

    #[test]
    fn runtime_config_sanitized_passes_through_valid_factors() {
        let cfg = RuntimeConfig {
            profile_thresholds: ProfileThresholds::DEFAULT,
            sppt_factor: 1.20,
            fppt_factor: 1.40,
            gpu_clock_fractions: GpuClockFractions::DEFAULT,
        };
        assert_eq!(cfg.clone().sanitized(), cfg);
    }

    #[test]
    fn runtime_config_sanitized_replaces_sub_unity_factors() {
        // Regression for Audit §2.3: a factor below 1.0 (e.g. `1.15`
        // fat-fingered as `.115`) would otherwise ask for SPPT/FPPT below
        // SPL — the reducer's floor clamp catches it defensively at apply
        // time, but the config layer should not hand out a broken value.
        let cfg = RuntimeConfig {
            profile_thresholds: ProfileThresholds::DEFAULT,
            sppt_factor: 0.115,
            fppt_factor: 0.0,
            gpu_clock_fractions: GpuClockFractions::DEFAULT,
        };
        let sanitized = cfg.sanitized();
        assert_eq!(sanitized.sppt_factor, RuntimeConfig::DEFAULT.sppt_factor);
        assert_eq!(sanitized.fppt_factor, RuntimeConfig::DEFAULT.fppt_factor);
    }

    #[test]
    fn runtime_config_sanitized_replaces_non_finite_factors() {
        let cfg = RuntimeConfig {
            profile_thresholds: ProfileThresholds::DEFAULT,
            sppt_factor: f32::NAN,
            fppt_factor: f32::INFINITY,
            gpu_clock_fractions: GpuClockFractions::DEFAULT,
        };
        let sanitized = cfg.sanitized();
        assert_eq!(sanitized.sppt_factor, RuntimeConfig::DEFAULT.sppt_factor);
        assert_eq!(sanitized.fppt_factor, RuntimeConfig::DEFAULT.fppt_factor);
    }
}
