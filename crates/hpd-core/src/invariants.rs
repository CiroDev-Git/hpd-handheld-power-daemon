// SPDX-License-Identifier: GPL-3.0-or-later

//! Cross-field invariants enforced before any [`crate::effect::Effect`]
//! is dispatched.

use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_error::HpdError;

/// Validates `FPPT ≥ SPPT ≥ SPL`. Called by every reducer branch that
/// produces a new [`PowerEnvelopeTarget`] before forwarding it as an
/// `ApplyPowerEnvelope` effect, so backends can assume the invariant
/// holds.
pub fn validate_power_envelope(target: &PowerEnvelopeTarget) -> Result<(), HpdError> {
    if target.sppt.0 < target.spl.0 {
        return Err(HpdError::InvariantViolation(format!(
            "SPPT ({}mW) must be >= SPL ({}mW)",
            target.sppt.0, target.spl.0
        )));
    }

    if let Some(fppt) = target.fppt {
        if fppt.0 < target.sppt.0 {
            return Err(HpdError::InvariantViolation(format!(
                "FPPT ({}mW) must be >= SPPT ({}mW)",
                fppt.0, target.sppt.0
            )));
        }
    }

    Ok(())
}
