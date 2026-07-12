// SPDX-License-Identifier: GPL-3.0-or-later

//! Cross-field invariants enforced before any [`crate::effect::Effect`]
//! is dispatched.

use hpd_capabilities::power::{PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_error::HpdError;

/// Validates `FPPT ≥ SPPT ≥ SPL` **and** every rail against the device's
/// own hardware range (`device_limits`). Called by every reducer branch
/// that produces a new [`PowerEnvelopeTarget`] before forwarding it as an
/// `ApplyPowerEnvelope` effect, so backends can assume the target is
/// fully within range and never has to reject a write itself.
///
/// The hardware-range checks matter beyond `Transition::SetSpl` (which
/// already checks SPL against `spl_min`/`spl_max` up front): the manual
/// `Transition::SetEnvelope` path takes a caller-supplied target with no
/// other bounds check at all. Found on-device (2026-07-12): the ASUS ROG
/// Xbox Ally X's SPPT/FPPT firmware attributes report their own
/// `min_value` *above* SPL's — a manually-supplied SPPT/FPPT that
/// satisfies the ordering invariant above can still undershoot the
/// hardware's real floor and get rejected by the firmware with `EINVAL`
/// at the I/O boundary instead of a clean `InvariantViolation` here.
pub fn validate_power_envelope(
    target: &PowerEnvelopeTarget,
    device_limits: &PowerEnvelopeLimits,
) -> Result<(), HpdError> {
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

    if target.spl.0 < device_limits.spl_min.0 || target.spl.0 > device_limits.spl_max.0 {
        return Err(HpdError::InvariantViolation(format!(
            "SPL ({}mW) out of hardware range ({}-{}mW)",
            target.spl.0, device_limits.spl_min.0, device_limits.spl_max.0
        )));
    }

    if target.sppt.0 < device_limits.sppt_min.0 || target.sppt.0 > device_limits.sppt_max.0 {
        return Err(HpdError::InvariantViolation(format!(
            "SPPT ({}mW) out of hardware range ({}-{}mW)",
            target.sppt.0, device_limits.sppt_min.0, device_limits.sppt_max.0
        )));
    }

    if let Some(fppt) = target.fppt {
        if fppt.0 < device_limits.fppt_min.0 || fppt.0 > device_limits.fppt_max.0 {
            return Err(HpdError::InvariantViolation(format!(
                "FPPT ({}mW) out of hardware range ({}-{}mW)",
                fppt.0, device_limits.fppt_min.0, device_limits.fppt_max.0
            )));
        }
    }

    Ok(())
}
