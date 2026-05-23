use serde::{Deserialize, Serialize};

use crate::error::HpdError;

/// Conversion factor between watts (kernel-facing) and milliwatts
/// (domain-facing). Centralised so the literal `1000` never appears in
/// power-conversion logic across the workspace.
pub const MILLIWATTS_PER_WATT: u32 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PowerMilliwatts(pub u32);

impl PowerMilliwatts {
    /// Build a `PowerMilliwatts` from an integer wattage. Returns
    /// `InvariantViolation` when `watts * 1000` would overflow `u32` — the
    /// same contract `Transition::SetSpl` previously enforced inline.
    pub fn from_watts(watts: u32) -> Result<Self, HpdError> {
        watts
            .checked_mul(MILLIWATTS_PER_WATT)
            .map(Self)
            .ok_or_else(|| {
                HpdError::InvariantViolation(format!(
                    "watts value {} too large to convert to milliwatts",
                    watts
                ))
            })
    }

    /// Truncating conversion to whole watts. Sub-watt precision is lost,
    /// which is intentional — every external surface (kernel sysfs, D-Bus,
    /// logs) expresses TDP in whole watts.
    pub const fn as_watts(self) -> u32 {
        self.0 / MILLIWATTS_PER_WATT
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rpm(pub u16);
