use std::fmt;
use std::str::FromStr;
use serde::{Deserialize, Serialize};

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
    PowerSaver,
    Balanced,
    Performance,
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

#[derive(Debug, Clone, PartialEq)]
pub struct ProfileThresholds {
    pub low_frac: f32,
    pub high_frac: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SystemPreset {
    Silent,
    Performance,
    Turbo,
}

impl FromStr for SystemPreset {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "silent" => Ok(SystemPreset::Silent),
            "performance" => Ok(SystemPreset::Performance),
            "turbo" => Ok(SystemPreset::Turbo),
            _ => Err(format!("Unknown preset: {}", s)),
        }
    }
}

#[cfg(test)]
mod tests {
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
            let parsed = s.parse::<ProfileName>()
                .unwrap_or_else(|e| panic!("roundtrip failed for {:?}: {}", p, e));
            assert_eq!(parsed, p, "Display/FromStr roundtrip broken for {:?}", p);
        }
    }

    #[test]
    fn profile_fromstr_accepts_acpi_aliases_and_case() {
        assert_eq!("quiet".parse::<ProfileName>().unwrap(), ProfileName::PowerSaver);
        assert_eq!("low-power".parse::<ProfileName>().unwrap(), ProfileName::PowerSaver);
        assert_eq!("POWER-SAVER".parse::<ProfileName>().unwrap(), ProfileName::PowerSaver);
        assert_eq!("Balanced".parse::<ProfileName>().unwrap(), ProfileName::Balanced);
        assert_eq!("PERFORMANCE".parse::<ProfileName>().unwrap(), ProfileName::Performance);
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
}
