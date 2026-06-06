// SPDX-License-Identifier: GPL-3.0-or-later

use hpd_capabilities::fan_curve::FanCurvePreset;
use hpd_capabilities::power::{PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_capabilities::profile::ProfileThresholds;

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
