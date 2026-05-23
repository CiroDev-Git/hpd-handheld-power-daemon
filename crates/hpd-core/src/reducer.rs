use tracing::info;

use hpd_capabilities::error::HpdError;
use hpd_capabilities::power::PowerEnvelopeLimits;
use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::{ProfileThresholds, SystemPreset};
use hpd_capabilities::units::PowerMilliwatts;

use crate::effect::Effect;
use crate::inference::infer_profile_from_spl;
use crate::invariants::validate_power_envelope;
use crate::state::ProfileState;
use crate::transition::Transition;

pub struct ReducerOutput {
    pub new_state: ProfileState,
    pub effects: Vec<Effect>,
}

/// Compute new state and required effects 
/// Pure function, doesn't interact with OS
pub fn reduce(
    state: &ProfileState,
    transition: Transition,
    device_limits: &PowerEnvelopeLimits,
    profile_thresholds: &ProfileThresholds,
) -> Result<ReducerOutput, HpdError> {
    let mut new_state = state.clone();
    let mut effects = Vec::new();

    match transition {

        Transition::SetPreset(preset) => {
            let min_w = device_limits.spl_min.0 / 1000;
            let max_w = device_limits.spl_max.0 / 1000;

            let target_watts = match preset {
                SystemPreset::Silent => min_w,
                // saturating_add: defensive against pathological device_limits.
                SystemPreset::Performance => min_w.saturating_add(max_w) / 2,
                SystemPreset::Turbo => max_w,
            };

            return reduce(state, Transition::SetSpl(target_watts), device_limits, profile_thresholds);
        }

        // -----------------------------------------------------
        // SMART MODE (get boost automatically)
        // -----------------------------------------------------
        Transition::SetSpl(watts) => {
            // checked_mul prevents wrap-around for huge user input (e.g., u32::MAX),
            // which would otherwise produce a small wrapped value that spuriously
            // passes the range check below.
            let spl_mw = watts.checked_mul(1000).ok_or_else(|| {
                HpdError::InvariantViolation(format!(
                    "watts value {} too large to convert to milliwatts",
                    watts
                ))
            })?;

            if spl_mw < device_limits.spl_min.0 || spl_mw > device_limits.spl_max.0 {
                return Err(HpdError::InvariantViolation(
                    format!("SPL ({}W) out of hardware range", watts)
                ));
            }

            let sppt_mw = ((spl_mw as f32 * 1.15) as u32).min(device_limits.sppt_max.0);
            let fppt_mw = ((spl_mw as f32 * 1.25) as u32).min(device_limits.fppt_max.0);

            let new_target = PowerEnvelopeTarget {
                spl: PowerMilliwatts(spl_mw),
                sppt: PowerMilliwatts(sppt_mw),
                fppt: Some(PowerMilliwatts(fppt_mw)),
            };

            validate_power_envelope(&new_target)?;

            return apply_target_and_profile(state, new_target, device_limits, profile_thresholds);
        }

        // -----------------------------------------------------
        // MANUAL MODE (user define values)
        // -----------------------------------------------------
        Transition::SetEnvelope(new_target) => {
            validate_power_envelope(&new_target)?;

            return apply_target_and_profile(state, new_target, device_limits, profile_thresholds);
        }

        Transition::SetProfile(new_profile) => {
            if new_state.active_profile != new_profile {
                new_state.active_profile = new_profile.clone();
                new_state.fan_follows_tdp = false;

                effects.push(Effect::ApplyPlatformProfile(new_profile));
                effects.push(Effect::PersistState);
            }
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

        Transition::AcPowerChanged(is_plugged) => {
            // Debounce: ignore no-op transitions.
            if state.is_ac_connected == is_plugged {
                return Ok(ReducerOutput {
                    new_state: state.clone(),
                    effects: vec![],
                });
            }

            let mut output = if is_plugged {
                info!(preset = "turbo", "Charger plugged: saving DC target and applying preset");
                let mut temp_output = reduce(state, Transition::SetPreset(SystemPreset::Turbo), device_limits, profile_thresholds)?;
                temp_output.new_state.last_dc_target = Some(state.power_target.clone());
                temp_output
            } else {
                if let Some(ref prev_target) = state.last_dc_target {
                    info!(action = "restore_previous", "Charger unplugged: restoring previous DC target");
                    reduce(state, Transition::SetEnvelope(prev_target.clone()), device_limits, profile_thresholds)?
                } else {
                    info!(preset = "performance", "Charger unplugged: applying default preset");
                    reduce(state, Transition::SetPreset(SystemPreset::Performance), device_limits, profile_thresholds)?
                }
            };

            output.new_state.is_ac_connected = is_plugged;

            // Persist even when power_target didn't actually change: we still
            // mutated last_dc_target / is_ac_connected and those must survive
            // a reboot. apply_target_and_profile only emits PersistState when
            // the target changed, so we top it up here if missing.
            if !output.effects.iter().any(|e| matches!(e, Effect::PersistState)) {
                output.effects.push(Effect::PersistState);
            }

            return Ok(output);

        }

        Transition::SystemResumed => {
            info!("System resumed: reapplying last known config");

            let mut effects = Vec::new();
            
            effects.push(Effect::ApplyPowerEnvelope(state.power_target.clone()));
            effects.push(Effect::ApplyPlatformProfile(state.active_profile.clone()));
            effects.push(Effect::ApplyChargeThreshold(state.charge_end_threshold));
            
            return Ok(ReducerOutput {
                new_state: state.clone(),
                effects,
            });
        }

        Transition::EnableFanAuto => {
            if !new_state.fan_follows_tdp {
                info!("Enabling auto cooling profile (follows TDP)");
                new_state.fan_follows_tdp = true;
                
                return reduce(
                    &new_state, 
                    Transition::SetEnvelope(new_state.power_target.clone()), 
                    device_limits, 
                    profile_thresholds
                );
            }
        }
    }

    Ok(ReducerOutput { new_state, effects })
}

fn apply_target_and_profile(
    current_state: &ProfileState,
    new_target: PowerEnvelopeTarget,
    device_limits: &PowerEnvelopeLimits,
    thresholds: &ProfileThresholds,
) -> Result<ReducerOutput, HpdError> {

    let mut new_state = current_state.clone();
    let mut effects = Vec::new();

    if new_state.power_target != new_target {

        new_state.power_target = new_target.clone();
        effects.push(Effect::ApplyPowerEnvelope(new_target.clone()));

        // Auto-Profile
        if new_state.fan_follows_tdp {
            let inferred_profile = infer_profile_from_spl(
                &new_target, 
                device_limits, 
                thresholds
            );

            if new_state.active_profile != inferred_profile {
                new_state.active_profile = inferred_profile.clone();
                effects.push(Effect::ApplyPlatformProfile(inferred_profile));
            }

        }

        effects.push(Effect::PersistState);
    }

    Ok(ReducerOutput { new_state, effects })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hpd_capabilities::power::{PowerEnvelopeTarget, PowerEnvelopeLimits};
    use hpd_capabilities::profile::{ProfileName, ProfileThresholds};
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
            charge_end_threshold: 80,
            fan_follows_tdp: true,
            last_dc_target: None
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
        let thresholds = ProfileThresholds { low_frac: 0.33, high_frac: 0.67 };

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
            &thresholds
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
        let thresholds = ProfileThresholds { low_frac: 0.33, high_frac: 0.67 };

        // Trying to increase to 30W (almost max capability), should infer Performance
        let high_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(30000),
            sppt: PowerMilliwatts(30000),
            fppt: Some(PowerMilliwatts(30000)),
        };

        let output = reduce(&state, Transition::SetEnvelope(high_target), &limits, &thresholds).unwrap();

        assert_eq!(output.new_state.active_profile, ProfileName::Performance);
        assert!(output.effects.contains(&Effect::ApplyPlatformProfile(ProfileName::Performance)));
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
        let thresholds = ProfileThresholds { low_frac: 0.33, high_frac: 0.67 };

        let result = reduce(&state, Transition::SetSpl(u32::MAX), &limits, &thresholds);

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
        let thresholds = ProfileThresholds { low_frac: 0.33, high_frac: 0.67 };

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
        };

        let output = reduce(&state, Transition::AcPowerChanged(true), &limits, &thresholds)
            .expect("AcPowerChanged should succeed");

        assert!(
            output.effects.iter().any(|e| matches!(e, Effect::PersistState)),
            "AcPowerChanged must emit PersistState even when target is unchanged; got effects={:?}",
            output.effects
        );
        assert_eq!(output.new_state.last_dc_target, Some(turbo_target));
        assert!(output.new_state.is_ac_connected);
    }
}