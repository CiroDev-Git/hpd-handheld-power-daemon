use std::str::FromStr;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfileName {
    PowerSaver,
    Balanced,
    Performance,
    Custom(String),
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