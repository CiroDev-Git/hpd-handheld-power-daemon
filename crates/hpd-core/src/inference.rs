use hpd_capabilities::power::{PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_capabilities::profile::{ProfileName, ProfileThresholds};

/// Get the best profile of PPD based of SPL 
/// Relative to max and min capacities of device
pub fn infer_profile_from_spl(
    target: &PowerEnvelopeTarget,
    limits: &PowerEnvelopeLimits,
    thresholds: &ProfileThresholds,
) -> ProfileName {
    let range = limits.spl_max.0.saturating_sub(limits.spl_min.0) as f32;
    let current_offset = target.spl.0.saturating_sub(limits.spl_min.0) as f32;

    // Avoid split by zero (cero unwrap, cero panic)
    let fraction = if range > 0.0 {
        current_offset / range
    } else {
        0.0
    };

    if fraction < thresholds.low_frac {
        ProfileName::PowerSaver
    } else if fraction < thresholds.high_frac {
        ProfileName::Balanced
    } else {
        ProfileName::Performance
    }
}