// SPDX-License-Identifier: GPL-3.0-or-later

use hpd_capabilities::fan_curve::FanCurvePreset;
use hpd_capabilities::gpu_clock::{GpuClockConstraints, GpuClockRange};
use hpd_capabilities::power::{PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_capabilities::profile::{GpuClockFractions, ProfileThresholds};

/// Infer the fan-curve preset to pair with a given SPL target, expressed
/// as a fraction of the device's `[spl_min, spl_max]` range:
///
/// * `fraction < low_frac`              -> [`FanCurvePreset::Silent`]
/// * `low_frac <= fraction < high_frac` -> [`FanCurvePreset::Balanced`]
/// * `high_frac <= fraction`            -> [`FanCurvePreset::Aggressive`]
///
/// This drives the **auto-cooling** behaviour (`fan_follows_tdp`): when a
/// TDP change comes through and auto-cooling is on, the reducer swaps the
/// fan curve to match the new power level. It deliberately does **not**
/// touch the ACPI `platform_profile` — that is an independent power lever
/// (defaults to `Performance`) so the SPL the user sets is the real,
/// usable limit instead of being clamped down by a `PowerSaver`/`quiet`
/// EPP, the way the old TDP-follows-profile coupling did.
///
/// # Degenerate range
///
/// If `spl_max <= spl_min` (so the range is zero or negative), returns
/// [`FanCurvePreset::Balanced`] as a safe default: without any SPL spread
/// we don't know whether to bias quiet or cool, so the middle curve is
/// the least surprising choice.
pub fn infer_fan_curve_from_spl(
    target: &PowerEnvelopeTarget,
    limits: &PowerEnvelopeLimits,
    thresholds: &ProfileThresholds,
) -> FanCurvePreset {
    let range = limits.spl_max.0.saturating_sub(limits.spl_min.0);
    if range == 0 {
        return FanCurvePreset::Balanced;
    }

    let current_offset = target.spl.0.saturating_sub(limits.spl_min.0);
    let fraction = current_offset as f32 / range as f32;

    if fraction < thresholds.low_frac {
        FanCurvePreset::Silent
    } else if fraction < thresholds.high_frac {
        FanCurvePreset::Balanced
    } else {
        FanCurvePreset::Aggressive
    }
}

/// Resolve the auto-inferred fan-curve tier (the *same* value
/// [`infer_fan_curve_from_spl`] already computed for the current SPL —
/// callers should reuse it, not re-derive it) to a concrete GPU clock
/// ceiling, as a fraction of the device's live `OD_RANGE` (see
/// `hpd_capabilities::gpu_clock`'s module docs on why this is
/// fraction-based rather than an absolute MHz value — it's what makes
/// the curated preset portable to a device with a different range).
/// `min_mhz` always stays the device's own reported floor.
///
/// The result is clamped to always be a valid (strictly increasing)
/// range even at the degenerate `frac == 0.0` extreme, so this never
/// hands the backend something [`GpuClockRange::validate_against`] would
/// reject.
pub fn gpu_clock_range_for_tier(
    tier: FanCurvePreset,
    constraints: &GpuClockConstraints,
    fractions: &GpuClockFractions,
) -> GpuClockRange {
    let frac = match tier {
        FanCurvePreset::Silent => fractions.silent_max_frac,
        FanCurvePreset::Balanced => fractions.balanced_max_frac,
        FanCurvePreset::Aggressive => fractions.aggressive_max_frac,
    };
    let span = constraints
        .range_max_mhz
        .saturating_sub(constraints.range_min_mhz);
    let ceiling = constraints.range_min_mhz + (span as f32 * frac).round() as u32;
    let max_mhz = ceiling
        .max(constraints.range_min_mhz.saturating_add(1))
        .min(constraints.range_max_mhz);
    GpuClockRange {
        min_mhz: constraints.range_min_mhz,
        max_mhz,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    fn constraints() -> GpuClockConstraints {
        GpuClockConstraints {
            range_min_mhz: 600,
            range_max_mhz: 2900,
        }
    }

    #[test]
    fn silent_tier_uses_silent_fraction() {
        let r = gpu_clock_range_for_tier(
            FanCurvePreset::Silent,
            &constraints(),
            &GpuClockFractions::DEFAULT,
        );
        assert_eq!(r.min_mhz, 600);
        // 600 + round(2300 * 0.55) = 600 + 1265 = 1865
        assert_eq!(r.max_mhz, 1865);
    }

    #[test]
    fn aggressive_tier_reaches_full_device_ceiling_with_default_fractions() {
        let r = gpu_clock_range_for_tier(
            FanCurvePreset::Aggressive,
            &constraints(),
            &GpuClockFractions::DEFAULT,
        );
        assert_eq!(r.max_mhz, constraints().range_max_mhz);
    }

    #[test]
    fn zero_fraction_still_yields_a_strictly_valid_range() {
        let fractions = GpuClockFractions {
            silent_max_frac: 0.0,
            balanced_max_frac: 0.0,
            aggressive_max_frac: 0.0,
        };
        let r = gpu_clock_range_for_tier(FanCurvePreset::Silent, &constraints(), &fractions);
        assert!(r.min_mhz < r.max_mhz);
        assert!(r.validate_against(&constraints()).is_ok());
    }

    #[test]
    fn every_tier_resolves_to_a_range_that_passes_validate_against() {
        for tier in [
            FanCurvePreset::Silent,
            FanCurvePreset::Balanced,
            FanCurvePreset::Aggressive,
        ] {
            let r = gpu_clock_range_for_tier(tier, &constraints(), &GpuClockFractions::DEFAULT);
            assert!(
                r.validate_against(&constraints()).is_ok(),
                "tier {tier:?} produced an invalid range: {r:?}"
            );
        }
    }
}
