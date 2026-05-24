// SPDX-License-Identifier: GPL-3.0-or-later

//! TDP power-envelope capability.

use crate::error::HpdError;
use crate::units::PowerMilliwatts;
use serde::{Deserialize, Serialize};

/// Hardware-reported upper bounds for each rail of the power envelope.
/// Values come straight from the kernel firmware-attribute leaves and
/// are treated as immutable for the lifetime of the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PowerEnvelopeLimits {
    /// Minimum SPL allowed by the platform.
    pub spl_min: PowerMilliwatts,
    /// Maximum SPL allowed by the platform.
    pub spl_max: PowerMilliwatts,
    /// Maximum SPPT (short-window boost) allowed by the platform.
    pub sppt_max: PowerMilliwatts,
    /// Maximum FPPT (fast/burst boost) allowed by the platform.
    pub fppt_max: PowerMilliwatts,
}

/// User-requested target for the power envelope. Must satisfy
/// `FPPT >= SPPT >= SPL` (validated by the L3 reducer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PowerEnvelopeTarget {
    /// Sustained power limit.
    pub spl: PowerMilliwatts,
    /// Short-window boost (≥ SPL).
    pub sppt: PowerMilliwatts,
    /// Fast/burst boost (≥ SPPT). `None` on platforms that do not
    /// expose a separate FPPT rail (e.g. some Lenovo handhelds).
    pub fppt: Option<PowerMilliwatts>,
}

/// Read and write the SPL / SPPT / FPPT power envelope.
pub trait PowerEnvelope: Send + Sync {
    /// Returns the hardware-reported limits of the envelope.
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError>;

    /// Returns the currently programmed envelope as reported by the kernel.
    fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError>;

    /// Writes a new envelope. The caller (L3 reducer) must validate the
    /// `FPPT ≥ SPPT ≥ SPL` invariant before invoking — the backend
    /// trusts its input.
    fn set_target(&self, target: &PowerEnvelopeTarget) -> Result<(), HpdError>;
}
