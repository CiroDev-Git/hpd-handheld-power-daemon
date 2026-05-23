use hpd_capabilities::power::{PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_capabilities::profile::{ProfileName, ProfileThresholds};

/// Infer the best ACPI platform/cooling profile for a given SPL target,
/// expressed as a fraction of the device's `[spl_min, spl_max]` range:
///
/// * `fraction < low_frac`              -> [`ProfileName::PowerSaver`]
/// * `low_frac <= fraction < high_frac` -> [`ProfileName::Balanced`]
/// * `high_frac <= fraction`            -> [`ProfileName::Performance`]
///
/// # Degenerate range
///
/// If `spl_max <= spl_min` (so the range is zero or negative), the
/// function returns [`ProfileName::Balanced`] as a safe default. This
/// is the same fallback the previous in-Executor duplicate used; the
/// rationale is that without any information about the SPL spread we
/// don't know whether we should bias toward cooling or quiet, so the
/// middle profile is the least surprising choice.
pub fn infer_profile_from_spl(
    target: &PowerEnvelopeTarget,
    limits: &PowerEnvelopeLimits,
    thresholds: &ProfileThresholds,
) -> ProfileName {
    let range = limits.spl_max.0.saturating_sub(limits.spl_min.0);
    if range == 0 {
        return ProfileName::Balanced;
    }

    let current_offset = target.spl.0.saturating_sub(limits.spl_min.0);
    let fraction = current_offset as f32 / range as f32;

    if fraction < thresholds.low_frac {
        ProfileName::PowerSaver
    } else if fraction < thresholds.high_frac {
        ProfileName::Balanced
    } else {
        ProfileName::Performance
    }
}
