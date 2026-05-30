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
}

impl Default for ProfileThresholds {
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

    /// When `true`, a platform-profile change also programs the matching
    /// fan-curve preset (PowerSaver→Silent, Balanced→Balanced,
    /// Performance→Aggressive), Armoury-Crate style. When `false`
    /// (default), the fan curve is an independent manual selection set
    /// via `hpdctl fan curve set`. Hot-reloadable.
    pub fan_curve_follows_profile: bool,
}

impl RuntimeConfig {
    /// Defaults match the historic in-reducer constants: 1.15/1.25 boost
    /// multipliers, 0.33/0.67 cooling-profile cut-offs, fan-curve follow
    /// off (the curve is a manual selection until the operator opts in).
    pub const DEFAULT: Self = Self {
        profile_thresholds: ProfileThresholds::DEFAULT,
        sppt_factor: 1.15,
        fppt_factor: 1.25,
        fan_curve_follows_profile: false,
    };
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Convenience preset for the TDP envelope.
///
/// `TdpPreset` selects a target wattage on the SPL rail; it is **not** the
/// same as [`ProfileName`], which selects the ACPI platform/cooling
/// profile. Both can be active independently:
///
/// | `TdpPreset` | Resulting SPL              | Typical platform profile (auto) |
/// |-------------|----------------------------|---------------------------------|
/// | `Eco`       | `spl_min`                  | `PowerSaver`                    |
/// | `Balanced`  | midpoint of min/max        | `Balanced`                      |
/// | `Max`       | `spl_max`                  | `Performance`                   |
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
}
