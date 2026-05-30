// SPDX-License-Identifier: GPL-3.0-or-later

//! Pure state-transition function.

use tracing::info;

use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
use hpd_capabilities::power::PowerEnvelopeLimits;
use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::{ProfileName, RuntimeConfig, TdpPreset};
use hpd_capabilities::units::PowerMilliwatts;
use hpd_error::HpdError;

use crate::effect::Effect;
use crate::inference::infer_profile_from_spl;
use crate::invariants::validate_power_envelope;
use crate::state::ProfileState;
use crate::transition::Transition;

/// Combined output of a single [`reduce`] call: the post-transition
/// state and the ordered list of side-effects the executor must dispatch
/// to honour it.
pub struct ReducerOutput {
    /// State after the transition has been applied.
    pub new_state: ProfileState,
    /// Side-effects (hardware writes, persistence) to be dispatched by
    /// the executor in order.
    pub effects: Vec<Effect>,
}

/// Compute new state and required effects.
///
/// Pure function — no I/O, no async, no globals. `config` is the
/// currently-active `RuntimeConfig` (cooling thresholds + SPPT/FPPT
/// boost multipliers); callers re-pass it on every invocation so the
/// reducer never has to look at static state. The `Executor` owns the
/// authoritative copy and swaps it on `Transition::ConfigReload`.
pub fn reduce(
    state: &ProfileState,
    transition: Transition,
    device_limits: &PowerEnvelopeLimits,
    config: &RuntimeConfig,
) -> Result<ReducerOutput, HpdError> {
    let mut new_state = state.clone();
    let mut effects = Vec::new();

    match transition {
        Transition::SetPreset(preset) => {
            let min_w = device_limits.spl_min.as_watts();
            let max_w = device_limits.spl_max.as_watts();

            let target_watts = match preset {
                TdpPreset::Eco => min_w,
                // saturating_add: defensive against pathological device_limits.
                TdpPreset::Balanced => min_w.saturating_add(max_w) / 2,
                TdpPreset::Max => max_w,
            };

            return reduce(
                state,
                Transition::SetSpl(target_watts),
                device_limits,
                config,
            );
        }

        // -----------------------------------------------------
        // SMART MODE (get boost automatically)
        // -----------------------------------------------------
        Transition::SetSpl(watts) => {
            // PowerMilliwatts::from_watts encapsulates the overflow check
            // that protects against wrap-around for huge user input.
            let spl = PowerMilliwatts::from_watts(watts)?;

            if spl < device_limits.spl_min || spl > device_limits.spl_max {
                return Err(HpdError::InvariantViolation(format!(
                    "SPL ({}W) out of hardware range",
                    watts
                )));
            }

            let sppt_mw =
                ((spl.0 as f32 * config.sppt_factor) as u32).min(device_limits.sppt_max.0);
            let fppt_mw =
                ((spl.0 as f32 * config.fppt_factor) as u32).min(device_limits.fppt_max.0);

            let new_target = PowerEnvelopeTarget {
                spl,
                sppt: PowerMilliwatts(sppt_mw),
                fppt: Some(PowerMilliwatts(fppt_mw)),
            };

            validate_power_envelope(&new_target)?;

            return apply_target_and_profile(state, new_target, device_limits, config);
        }

        // -----------------------------------------------------
        // MANUAL MODE (user define values)
        // -----------------------------------------------------
        Transition::SetEnvelope(new_target) => {
            validate_power_envelope(&new_target)?;

            return apply_target_and_profile(state, new_target, device_limits, config);
        }

        Transition::SetProfile(new_profile) => {
            if new_state.active_profile != new_profile {
                new_state.active_profile = new_profile.clone();
                new_state.fan_follows_tdp = false;

                effects.push(Effect::ApplyPlatformProfile(new_profile));
                // Hybrid: when the curve tracks the profile, swap it too.
                reassert_curve_after_profile(&mut new_state, &mut effects, config);
                effects.push(Effect::PersistState);
            }
        }

        Transition::SetCoolingLevel(preset) => {
            // Unified lever: drive the platform profile and the fan curve
            // together to the requested level, and latch manual mode.
            let profile = profile_for_cooling(preset);
            let selection = FanCurveSelection::Preset(preset);
            new_state.fan_follows_tdp = false;
            if new_state.active_profile != profile {
                new_state.active_profile = profile.clone();
                effects.push(Effect::ApplyPlatformProfile(profile));
            }
            // Always (re)apply the curve — after the profile write, which
            // can reset the EC — so the two never drift apart.
            new_state.active_fan_curve = Some(selection);
            effects.push(Effect::ApplyFanCurve(selection));
            effects.push(Effect::PersistState);
        }

        Transition::ChargeThresholdChanged(threshold) => {
            if new_state.charge_end_threshold != threshold {
                new_state.charge_end_threshold = threshold;
                effects.push(Effect::ApplyChargeThreshold(threshold));
                effects.push(Effect::PersistState);
            }
        }

        Transition::SyncPowerTarget(real_target) => {
            // Forced rollback. Kernel overrode (or rejected) the hpd config.
            // PropertiesChanged is emitted automatically by the daemon's
            // properties watcher when state_tx receives this new value.
            new_state.power_target = real_target;
        }

        Transition::SyncPlatformProfile(real_profile) => {
            // Mirror of SyncPowerTarget for the platform profile rail.
            // No PersistState here either: the executor reads the
            // authoritative value back from hardware, so a reboot
            // would re-read the same value and converge anyway.
            new_state.active_profile = real_profile;
        }

        Transition::SyncChargeThreshold(real_threshold) => {
            // Mirror of SyncPowerTarget for the charge end threshold.
            new_state.charge_end_threshold = real_threshold;
        }

        Transition::AcPowerChanged(is_plugged) => {
            // Debounce: ignore no-op transitions.
            if state.is_ac_connected == is_plugged {
                return Ok(ReducerOutput {
                    new_state: state.clone(),
                    effects: vec![],
                });
            }

            let mut output = if is_plugged {
                info!(preset = %TdpPreset::Max, "Charger plugged: saving DC target and applying preset");
                let mut temp_output = reduce(
                    state,
                    Transition::SetPreset(TdpPreset::Max),
                    device_limits,
                    config,
                )?;
                temp_output.new_state.last_dc_target = Some(state.power_target.clone());
                temp_output
            } else if let Some(ref prev_target) = state.last_dc_target {
                info!(
                    action = "restore_previous",
                    "Charger unplugged: restoring previous DC target"
                );
                reduce(
                    state,
                    Transition::SetEnvelope(prev_target.clone()),
                    device_limits,
                    config,
                )?
            } else {
                info!(preset = %TdpPreset::Balanced, "Charger unplugged: applying default preset");
                reduce(
                    state,
                    Transition::SetPreset(TdpPreset::Balanced),
                    device_limits,
                    config,
                )?
            };

            output.new_state.is_ac_connected = is_plugged;

            // Persist even when power_target didn't actually change: we still
            // mutated last_dc_target / is_ac_connected and those must survive
            // a reboot. apply_target_and_profile only emits PersistState when
            // the target changed, so we top it up here if missing.
            if !output
                .effects
                .iter()
                .any(|e| matches!(e, Effect::PersistState))
            {
                output.effects.push(Effect::PersistState);
            }

            return Ok(output);
        }

        Transition::SystemResumed => {
            info!("System resumed: reapplying last known config");
            let mut effects = vec![
                Effect::ApplyPowerEnvelope(state.power_target.clone()),
                Effect::ApplyPlatformProfile(state.active_profile.clone()),
                Effect::ApplyChargeThreshold(state.charge_end_threshold),
            ];
            // Re-apply the managed fan curve: the EC can drop or reset
            // the custom curve across suspend (the "fans blast at 100%
            // on resume" bug). If we are not managing the curve
            // (`None`), leave firmware auto alone.
            if let Some(ref selection) = state.active_fan_curve {
                effects.push(Effect::ApplyFanCurve(*selection));
            }
            return Ok(ReducerOutput {
                new_state: state.clone(),
                effects,
            });
        }

        Transition::SetFanCurve(selection) => {
            if new_state.active_fan_curve != Some(selection) {
                new_state.active_fan_curve = Some(selection);
                effects.push(Effect::ApplyFanCurve(selection));
                effects.push(Effect::PersistState);
            }
        }

        Transition::ResetFanCurve => {
            if new_state.active_fan_curve.is_some() {
                new_state.active_fan_curve = None;
                effects.push(Effect::ResetFanCurve);
                effects.push(Effect::PersistState);
            }
        }

        Transition::EnableFanAuto => {
            if !new_state.fan_follows_tdp {
                info!("Enabling auto cooling profile (follows TDP)");
                new_state.fan_follows_tdp = true;

                return reduce(
                    &new_state,
                    Transition::SetEnvelope(new_state.power_target.clone()),
                    device_limits,
                    config,
                );
            }
        }

        // Intercepted by the Executor before reduce() is called; the
        // reducer treats it as a no-op so isolated calls (tests) are safe.
        Transition::ConfigReload(_) => {}

        Transition::Shutdown => {
            // Final flush before exit: persist state without mutating it.
            // The Executor breaks its run() loop after the resulting
            // PersistState effect has been dispatched.
            info!("Shutdown requested: persisting state before exit");
            effects.push(Effect::PersistState);
        }
    }

    Ok(ReducerOutput { new_state, effects })
}

fn apply_target_and_profile(
    current_state: &ProfileState,
    new_target: PowerEnvelopeTarget,
    device_limits: &PowerEnvelopeLimits,
    config: &RuntimeConfig,
) -> Result<ReducerOutput, HpdError> {
    let mut new_state = current_state.clone();
    let mut effects = Vec::new();

    if new_state.power_target != new_target {
        new_state.power_target = new_target.clone();
        effects.push(Effect::ApplyPowerEnvelope(new_target.clone()));

        // Auto-Profile
        if new_state.fan_follows_tdp {
            let inferred_profile =
                infer_profile_from_spl(&new_target, device_limits, &config.profile_thresholds);

            if new_state.active_profile != inferred_profile {
                new_state.active_profile = inferred_profile.clone();
                effects.push(Effect::ApplyPlatformProfile(inferred_profile));
                // Hybrid: keep the fan curve in lock-step with the
                // inferred cooling profile when the operator opted in.
                reassert_curve_after_profile(&mut new_state, &mut effects, config);
            }
        }

        effects.push(Effect::PersistState);
    }

    Ok(ReducerOutput { new_state, effects })
}

/// Map a unified cooling level (expressed as a fan-curve preset) to the
/// platform profile it pairs with. Inverse of
/// [`curve_preset_for_profile`] over the three canonical levels.
fn profile_for_cooling(preset: FanCurvePreset) -> ProfileName {
    match preset {
        FanCurvePreset::Silent => ProfileName::PowerSaver,
        FanCurvePreset::Balanced => ProfileName::Balanced,
        FanCurvePreset::Aggressive => ProfileName::Performance,
    }
}

/// Map a platform profile to its companion fan-curve preset. `Custom`
/// vendor profiles have no canonical curve, so they leave the fan curve
/// untouched.
fn curve_preset_for_profile(profile: &ProfileName) -> Option<FanCurvePreset> {
    match profile {
        ProfileName::PowerSaver => Some(FanCurvePreset::Silent),
        ProfileName::Balanced => Some(FanCurvePreset::Balanced),
        ProfileName::Performance => Some(FanCurvePreset::Aggressive),
        ProfileName::Custom(_) => None,
    }
}

/// Re-assert the fan curve after a platform-profile change. Call this
/// whenever an `ApplyPlatformProfile` effect was just emitted.
///
/// Two jobs, both ending in an unconditional `ApplyFanCurve`:
///
/// * **Follow** (`fan_curve_follows_profile` on) — switch the active
///   curve to the preset matching the new profile and re-apply it.
/// * **Preserve** (the default) — re-apply the *currently active* curve
///   unchanged. Writing the ACPI `platform_profile` can make the EC drop
///   the custom curve back to its automatic mode (the same failure mode
///   as suspend/resume), so we must re-write our curve afterwards or it
///   is silently lost. A no-op when no curve is managed (`None`).
///
/// The re-apply is intentionally unconditional (not gated on "selection
/// changed") precisely because the EC may have reset a curve that is, by
/// our bookkeeping, unchanged. The caller owns the surrounding
/// `PersistState`.
fn reassert_curve_after_profile(
    new_state: &mut ProfileState,
    effects: &mut Vec<Effect>,
    config: &RuntimeConfig,
) {
    if config.fan_curve_follows_profile {
        if let Some(preset) = curve_preset_for_profile(&new_state.active_profile) {
            let selection = FanCurveSelection::Preset(preset);
            new_state.active_fan_curve = Some(selection);
            effects.push(Effect::ApplyFanCurve(selection));
            return;
        }
        // Custom vendor profile has no companion preset — fall through
        // and preserve whatever curve is currently active.
    }
    if let Some(selection) = new_state.active_fan_curve {
        effects.push(Effect::ApplyFanCurve(selection));
    }
}

#[cfg(test)]
mod tests {
    // Test code may use `.unwrap()` / `.expect()` / `panic!` freely;
    // the strict bar in `[workspace.lints.clippy]` applies to
    // production code only.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_capabilities::charge::DEFAULT_CHARGE_THRESHOLD;
    use hpd_capabilities::power::{PowerEnvelopeLimits, PowerEnvelopeTarget};
    use hpd_capabilities::profile::{ProfileName, RuntimeConfig};
    use hpd_capabilities::units::PowerMilliwatts;

    fn setup_state() -> ProfileState {
        ProfileState {
            power_target: PowerEnvelopeTarget {
                spl: PowerMilliwatts(15000),
                sppt: PowerMilliwatts(15000),
                fppt: Some(PowerMilliwatts(15000)),
            },
            active_profile: ProfileName::Balanced,
            is_ac_connected: false,
            charge_end_threshold: DEFAULT_CHARGE_THRESHOLD,
            fan_follows_tdp: true,
            last_dc_target: None,
            active_fan_curve: None,
        }
    }

    #[test]
    fn test_invariant_fppt_sppt_spl() {
        let state = setup_state();
        let limits = PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000), // Ally X ranges
            sppt_max: PowerMilliwatts(43000),
            fppt_max: PowerMilliwatts(53000),
        };
        let config = RuntimeConfig::DEFAULT;

        // Invalid attempt: SPPT lower than SPL
        let bad_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(20000),
            sppt: PowerMilliwatts(15000),
            fppt: Some(PowerMilliwatts(25000)),
        };

        let result = reduce(
            &state,
            Transition::SetEnvelope(bad_target),
            &limits,
            &config,
        );

        assert!(result.is_err(), "Must fail because SPPT < SPL");
    }

    #[test]
    fn test_profile_inference() {
        let state = setup_state();
        let limits = PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000),
            sppt_max: PowerMilliwatts(43000),
            fppt_max: PowerMilliwatts(53000),
        };
        let config = RuntimeConfig::DEFAULT;

        // Trying to increase to 30W (almost max capability), should infer Performance
        let high_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(30000),
            sppt: PowerMilliwatts(30000),
            fppt: Some(PowerMilliwatts(30000)),
        };

        let output = reduce(
            &state,
            Transition::SetEnvelope(high_target),
            &limits,
            &config,
        )
        .unwrap();

        assert_eq!(output.new_state.active_profile, ProfileName::Performance);
        assert!(output
            .effects
            .contains(&Effect::ApplyPlatformProfile(ProfileName::Performance)));
    }

    #[test]
    fn test_set_spl_overflow_rejected() {
        // Regression for Audit §3.3 / Lote 5: `watts * 1000` could wrap
        // around for huge user input, producing a small value that would
        // spuriously pass the range check below it.
        let state = setup_state();
        let limits = PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000),
            sppt_max: PowerMilliwatts(43000),
            fppt_max: PowerMilliwatts(55000),
        };
        let config = RuntimeConfig::DEFAULT;

        let result = reduce(&state, Transition::SetSpl(u32::MAX), &limits, &config);

        assert!(
            matches!(result, Err(HpdError::InvariantViolation(_))),
            "u32::MAX watts must be rejected as overflow, got {:?}",
            result.err()
        );
    }

    #[test]
    fn test_ac_plugged_persists_last_dc_target_even_when_target_unchanged() {
        // Regression for Audit §3.5 / Lote 6: when the system is already at
        // the Turbo target (e.g., a stale boot-time state) and the charger is
        // plugged in, apply_target_and_profile sees no envelope change and
        // skips PersistState. But we DO mutate last_dc_target and
        // is_ac_connected, so we MUST persist or they're lost on reboot.
        let limits = PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000),
            sppt_max: PowerMilliwatts(43000),
            fppt_max: PowerMilliwatts(55000),
        };
        let config = RuntimeConfig::DEFAULT;

        // Start with the exact target that AcPowerChanged(true) -> SetPreset(Turbo)
        // would produce (35W SPL, sppt = 35000*1.15 = 40250, fppt = 35000*1.25 = 43750).
        // apply_target_and_profile sees no change and emits no PersistState.
        let turbo_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(35_000),
            sppt: PowerMilliwatts(40_250),
            fppt: Some(PowerMilliwatts(43_750)),
        };
        let state = ProfileState {
            power_target: turbo_target.clone(),
            active_profile: ProfileName::Performance,
            is_ac_connected: false,
            charge_end_threshold: 80,
            fan_follows_tdp: true,
            last_dc_target: None,
            active_fan_curve: None,
        };

        let output = reduce(&state, Transition::AcPowerChanged(true), &limits, &config)
            .expect("AcPowerChanged should succeed");

        assert!(
            output
                .effects
                .iter()
                .any(|e| matches!(e, Effect::PersistState)),
            "AcPowerChanged must emit PersistState even when target is unchanged; got effects={:?}",
            output.effects
        );
        assert_eq!(output.new_state.last_dc_target, Some(turbo_target));
        assert!(output.new_state.is_ac_connected);
    }

    fn setup_limits() -> PowerEnvelopeLimits {
        PowerEnvelopeLimits {
            spl_min: PowerMilliwatts(7000),
            spl_max: PowerMilliwatts(35000),
            sppt_max: PowerMilliwatts(43000),
            fppt_max: PowerMilliwatts(55000),
        }
    }

    fn setup_config() -> RuntimeConfig {
        RuntimeConfig::DEFAULT
    }

    // ---------- AcPowerChanged ----------

    #[test]
    fn test_ac_changed_is_debounced_when_already_in_same_state() {
        // Redundant AcPowerChanged(false) on an already-DC state must yield zero
        // effects (no spurious re-apply / persistence).
        let state = setup_state();
        let out = reduce(
            &state,
            Transition::AcPowerChanged(false),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert!(
            out.effects.is_empty(),
            "expected no effects, got {:?}",
            out.effects
        );
        assert_eq!(out.new_state, state);
    }

    #[test]
    fn test_ac_plugged_saves_dc_target_and_applies_max_preset() {
        // Charger goes in: snapshot the current DC target and ramp SPL to spl_max.
        let state = setup_state(); // DC, target (15,15,15)
        let out = reduce(
            &state,
            Transition::AcPowerChanged(true),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert!(out.new_state.is_ac_connected);
        assert_eq!(
            out.new_state.last_dc_target,
            Some(state.power_target.clone())
        );
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(35_000));
        assert!(out.effects.contains(&Effect::PersistState));
    }

    #[test]
    fn test_ac_unplugged_restores_saved_dc_target() {
        // Unplug with a remembered DC target: restore that exact envelope.
        let saved = PowerEnvelopeTarget {
            spl: PowerMilliwatts(10_000),
            sppt: PowerMilliwatts(12_000),
            fppt: Some(PowerMilliwatts(14_000)),
        };
        let mut state = setup_state();
        state.is_ac_connected = true;
        state.last_dc_target = Some(saved.clone());

        let out = reduce(
            &state,
            Transition::AcPowerChanged(false),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert!(!out.new_state.is_ac_connected);
        assert_eq!(out.new_state.power_target, saved);
        assert!(out.effects.contains(&Effect::PersistState));
    }

    #[test]
    fn test_ac_unplugged_without_saved_target_applies_balanced_preset() {
        // Cold case: no DC memory. Fall back to the Balanced preset, which on
        // [7,35]W is midpoint = 21W.
        let mut state = setup_state();
        state.is_ac_connected = true;
        state.last_dc_target = None;

        let out = reduce(
            &state,
            Transition::AcPowerChanged(false),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert!(!out.new_state.is_ac_connected);
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(21_000));
    }

    // ---------- SystemResumed ----------

    #[test]
    fn test_system_resumed_reapplies_envelope_profile_and_threshold() {
        // After resume the kernel may have lost our config. The reducer must
        // emit exactly the three re-apply effects without mutating state.
        let state = setup_state();
        let out = reduce(
            &state,
            Transition::SystemResumed,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert_eq!(out.effects.len(), 3);
        assert!(out
            .effects
            .contains(&Effect::ApplyPowerEnvelope(state.power_target.clone())));
        assert!(out
            .effects
            .contains(&Effect::ApplyPlatformProfile(state.active_profile.clone())));
        assert!(out
            .effects
            .contains(&Effect::ApplyChargeThreshold(state.charge_end_threshold)));
    }

    // ---------- EnableFanAuto ----------

    #[test]
    fn test_enable_fan_auto_flips_flag_when_previously_disabled() {
        // Today the re-evaluation produces zero effects because the inferred
        // profile is only re-applied behind an envelope change in
        // apply_target_and_profile; the contract for this transition is just
        // that the flag flips. Persistence of the flag itself is a separate
        // concern tracked in the audit.
        let mut state = setup_state();
        state.fan_follows_tdp = false;
        let out = reduce(
            &state,
            Transition::EnableFanAuto,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert!(out.new_state.fan_follows_tdp);
    }

    #[test]
    fn test_enable_fan_auto_is_no_op_when_already_enabled() {
        let state = setup_state(); // fan_follows_tdp = true
        let out = reduce(
            &state,
            Transition::EnableFanAuto,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert!(out.effects.is_empty());
    }

    // ---------- ChargeThresholdChanged ----------

    #[test]
    fn test_charge_threshold_changed_applies_and_persists() {
        let state = setup_state(); // threshold = 80
        let out = reduce(
            &state,
            Transition::ChargeThresholdChanged(60),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.charge_end_threshold, 60);
        assert_eq!(
            out.effects,
            vec![Effect::ApplyChargeThreshold(60), Effect::PersistState]
        );
    }

    #[test]
    fn test_charge_threshold_changed_is_no_op_when_unchanged() {
        let state = setup_state(); // threshold = 80
        let out = reduce(
            &state,
            Transition::ChargeThresholdChanged(80),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert!(out.effects.is_empty());
    }

    // ---------- SetSpl boundaries ----------

    #[test]
    fn test_set_spl_just_below_min_is_rejected() {
        let state = setup_state();
        let result = reduce(
            &state,
            Transition::SetSpl(6),
            &setup_limits(),
            &setup_config(),
        );
        assert!(matches!(result, Err(HpdError::InvariantViolation(_))));
    }

    #[test]
    fn test_set_spl_just_above_max_is_rejected() {
        let state = setup_state();
        let result = reduce(
            &state,
            Transition::SetSpl(36),
            &setup_limits(),
            &setup_config(),
        );
        assert!(matches!(result, Err(HpdError::InvariantViolation(_))));
    }

    #[test]
    fn test_set_spl_at_min_boundary_is_accepted() {
        let state = setup_state();
        let out = reduce(
            &state,
            Transition::SetSpl(7),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(7_000));
    }

    #[test]
    fn test_set_spl_at_max_boundary_is_accepted() {
        let state = setup_state();
        let out = reduce(
            &state,
            Transition::SetSpl(35),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.power_target.spl, PowerMilliwatts(35_000));
    }

    // ---------- SetEnvelope invariant (FPPT >= SPPT) ----------

    #[test]
    fn test_set_envelope_rejects_fppt_below_sppt() {
        // Complement to test_invariant_fppt_sppt_spl (SPPT < SPL): this covers
        // the second invariant — FPPT < SPPT — in validate_power_envelope.
        let state = setup_state();
        let bad = PowerEnvelopeTarget {
            spl: PowerMilliwatts(15_000),
            sppt: PowerMilliwatts(20_000),
            fppt: Some(PowerMilliwatts(18_000)),
        };
        let result = reduce(
            &state,
            Transition::SetEnvelope(bad),
            &setup_limits(),
            &setup_config(),
        );
        assert!(matches!(result, Err(HpdError::InvariantViolation(_))));
    }

    // ---------- Sync* (rollback paths) ----------

    #[test]
    fn test_sync_power_target_overwrites_state_without_side_effects() {
        // Rollback: trust the kernel's view, mutate in-memory state, but do
        // NOT emit ApplyPowerEnvelope (that would loop the executor).
        let state = setup_state();
        let real = PowerEnvelopeTarget {
            spl: PowerMilliwatts(12_000),
            sppt: PowerMilliwatts(13_800),
            fppt: Some(PowerMilliwatts(15_000)),
        };
        let out = reduce(
            &state,
            Transition::SyncPowerTarget(real.clone()),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.power_target, real);
        assert!(
            out.effects.is_empty(),
            "rollback must not produce effects, got {:?}",
            out.effects
        );
    }

    #[test]
    fn test_sync_platform_profile_overwrites_state_without_side_effects() {
        // Mirror of SyncPowerTarget for the platform profile rail
        // (Lote 38 / Audit V2 §4.5.1).
        let mut state = setup_state();
        state.active_profile = ProfileName::Performance;
        let out = reduce(
            &state,
            Transition::SyncPlatformProfile(ProfileName::PowerSaver),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.active_profile, ProfileName::PowerSaver);
        assert!(
            out.effects.is_empty(),
            "rollback must not produce effects, got {:?}",
            out.effects
        );
    }

    #[test]
    fn test_sync_charge_threshold_overwrites_state_without_side_effects() {
        // Mirror of SyncPowerTarget for the charge end threshold
        // (Lote 38 / Audit V2 §4.5.1).
        let state = setup_state();
        assert_ne!(state.charge_end_threshold, 65);
        let out = reduce(
            &state,
            Transition::SyncChargeThreshold(65),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.charge_end_threshold, 65);
        assert!(
            out.effects.is_empty(),
            "rollback must not produce effects, got {:?}",
            out.effects
        );
    }

    // ---------- Shutdown ----------

    #[test]
    fn test_shutdown_emits_only_persist_state_and_leaves_state_untouched() {
        // Last-chance flush before exit: the reducer must NOT mutate
        // ProfileState (we want exactly what was already in memory to
        // hit the disk) and must emit a single PersistState effect that
        // the Executor will dispatch before breaking its run() loop.
        let state = setup_state();
        let out = reduce(
            &state,
            Transition::Shutdown,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(
            out.new_state, state,
            "Shutdown must not mutate ProfileState"
        );
        assert_eq!(out.effects, vec![Effect::PersistState]);
    }

    // ---------- Fan curve ----------

    #[test]
    fn test_set_fan_curve_applies_and_persists_when_changed() {
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let state = setup_state(); // active_fan_curve = None
        let sel = FanCurveSelection::Preset(FanCurvePreset::Balanced);
        let out = reduce(
            &state,
            Transition::SetFanCurve(sel),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.active_fan_curve, Some(sel));
        assert_eq!(
            out.effects,
            vec![Effect::ApplyFanCurve(sel), Effect::PersistState]
        );
    }

    #[test]
    fn test_set_fan_curve_is_no_op_when_unchanged() {
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let sel = FanCurveSelection::Preset(FanCurvePreset::Aggressive);
        let mut state = setup_state();
        state.active_fan_curve = Some(sel);
        let out = reduce(
            &state,
            Transition::SetFanCurve(sel),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert!(out.effects.is_empty());
    }

    #[test]
    fn test_reset_fan_curve_clears_and_emits_reset() {
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let mut state = setup_state();
        state.active_fan_curve = Some(FanCurveSelection::Preset(FanCurvePreset::Silent));
        let out = reduce(
            &state,
            Transition::ResetFanCurve,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state.active_fan_curve, None);
        assert_eq!(
            out.effects,
            vec![Effect::ResetFanCurve, Effect::PersistState]
        );
    }

    #[test]
    fn test_reset_fan_curve_is_no_op_when_already_auto() {
        let state = setup_state(); // active_fan_curve = None
        let out = reduce(
            &state,
            Transition::ResetFanCurve,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert!(out.effects.is_empty());
    }

    #[test]
    fn test_system_resumed_reapplies_fan_curve_when_managed() {
        // Regression for the suspend/resume bug: a managed curve must be
        // re-applied on resume (the EC can reset it across suspend),
        // adding a 4th effect on top of the envelope/profile/charge trio.
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let sel = FanCurveSelection::Preset(FanCurvePreset::Balanced);
        let mut state = setup_state();
        state.active_fan_curve = Some(sel);
        let out = reduce(
            &state,
            Transition::SystemResumed,
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert_eq!(out.effects.len(), 4);
        assert!(out.effects.contains(&Effect::ApplyFanCurve(sel)));
    }

    #[test]
    fn test_set_profile_follows_curve_when_enabled() {
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let state = setup_state(); // active_profile = Balanced, curve = None
        let mut config = setup_config();
        config.fan_curve_follows_profile = true;

        let out = reduce(
            &state,
            Transition::SetProfile(ProfileName::Performance),
            &setup_limits(),
            &config,
        )
        .unwrap();

        assert_eq!(out.new_state.active_profile, ProfileName::Performance);
        assert_eq!(
            out.new_state.active_fan_curve,
            Some(FanCurveSelection::Preset(FanCurvePreset::Aggressive))
        );
        assert!(out
            .effects
            .contains(&Effect::ApplyFanCurve(FanCurveSelection::Preset(
                FanCurvePreset::Aggressive
            ))));
    }

    #[test]
    fn test_set_profile_does_not_touch_curve_when_follow_disabled() {
        // With fan_curve_follows_profile explicitly off and no managed
        // curve, a profile change must leave the fan curve alone.
        let state = setup_state(); // active_fan_curve = None
        let mut config = setup_config();
        config.fan_curve_follows_profile = false;
        let out = reduce(
            &state,
            Transition::SetProfile(ProfileName::Performance),
            &setup_limits(),
            &config,
        )
        .unwrap();
        assert_eq!(out.new_state.active_fan_curve, None);
        assert!(!out
            .effects
            .iter()
            .any(|e| matches!(e, Effect::ApplyFanCurve(_))));
    }

    #[test]
    fn test_set_cooling_level_sets_profile_and_curve_together() {
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let mut state = setup_state(); // Balanced profile, fan_follows_tdp = true
        state.fan_follows_tdp = true;
        let out = reduce(
            &state,
            Transition::SetCoolingLevel(FanCurvePreset::Aggressive),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        // Profile + curve both move to the aggressive level, and mode latches manual.
        assert_eq!(out.new_state.active_profile, ProfileName::Performance);
        assert_eq!(
            out.new_state.active_fan_curve,
            Some(FanCurveSelection::Preset(FanCurvePreset::Aggressive))
        );
        assert!(!out.new_state.fan_follows_tdp);
        assert!(out
            .effects
            .contains(&Effect::ApplyPlatformProfile(ProfileName::Performance)));
        assert!(out
            .effects
            .contains(&Effect::ApplyFanCurve(FanCurveSelection::Preset(
                FanCurvePreset::Aggressive
            ))));
        // Curve write comes AFTER the profile write.
        let p = out
            .effects
            .iter()
            .position(|e| matches!(e, Effect::ApplyPlatformProfile(_)))
            .unwrap();
        let c = out
            .effects
            .iter()
            .position(|e| matches!(e, Effect::ApplyFanCurve(_)))
            .unwrap();
        assert!(c > p);
    }

    #[test]
    fn test_profile_change_reasserts_active_curve_even_with_follow_disabled() {
        // Preservation: writing the platform profile can make the EC drop
        // the custom curve, so a profile change must re-apply the active
        // curve UNCHANGED even when fan_curve_follows_profile is off.
        use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
        let sel = FanCurveSelection::Preset(FanCurvePreset::Silent);
        let mut state = setup_state();
        state.active_fan_curve = Some(sel);
        let mut config = setup_config();
        config.fan_curve_follows_profile = false; // preservation path, not follow
        let out = reduce(
            &state,
            Transition::SetProfile(ProfileName::Performance),
            &setup_limits(),
            &config,
        )
        .unwrap();
        // Curve selection is preserved, not changed to a matching preset.
        assert_eq!(out.new_state.active_fan_curve, Some(sel));
        // But it IS re-applied to the hardware after the profile write.
        assert!(out.effects.contains(&Effect::ApplyFanCurve(sel)));
        // Ordering: the curve re-apply comes after the profile write.
        let profile_idx = out
            .effects
            .iter()
            .position(|e| matches!(e, Effect::ApplyPlatformProfile(_)))
            .unwrap();
        let curve_idx = out
            .effects
            .iter()
            .position(|e| matches!(e, Effect::ApplyFanCurve(_)))
            .unwrap();
        assert!(
            curve_idx > profile_idx,
            "curve must be re-applied AFTER the profile"
        );
    }

    // ---------- ConfigReload (no-op at the reducer layer) ----------

    #[test]
    fn test_config_reload_is_a_no_op_in_the_reducer() {
        // The Executor intercepts ConfigReload before reduce() to mutate
        // its own RuntimeConfig. Calling reduce() with it directly must
        // therefore touch neither state nor effects — otherwise an
        // integration that bypasses the Executor (e.g. a future
        // synchronous test harness) would silently corrupt state.
        let state = setup_state();
        let out = reduce(
            &state,
            Transition::ConfigReload(RuntimeConfig::DEFAULT),
            &setup_limits(),
            &setup_config(),
        )
        .unwrap();
        assert_eq!(out.new_state, state);
        assert!(out.effects.is_empty());
    }
}
