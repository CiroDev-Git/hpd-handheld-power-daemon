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
            let target_watts = match preset {
                // Silent: 10W (or min value of device if its greater than 10)
                SystemPreset::Silent => 10.max(device_limits.spl_min.0 / 1000),
                // Performance: 15W
                SystemPreset::Performance => 15,
                // Turbo: 30W (or max value of device if its lower than 30)
                SystemPreset::Turbo => 30.min(device_limits.spl_max.0 / 1000),
            };
            return reduce(state, Transition::SetSpl(target_watts), device_limits, profile_thresholds);
        }

        // -----------------------------------------------------
        // SMART MODE (get boost automatically)
        // -----------------------------------------------------
        Transition::SetSpl(watts) => {
            let spl_mw = watts * 1000;
            
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
                effects.push(Effect::EmitDbusPropertiesChanged);
            }
        }

        Transition::AcChanged(is_ac) => {
            if new_state.is_ac_connected != is_ac {
                new_state.is_ac_connected = is_ac;
                effects.push(Effect::EmitDbusPropertiesChanged);
            }
        }

        Transition::ChargeThresholdChanged(threshold) => {
            if new_state.charge_end_threshold != threshold {
                new_state.charge_end_threshold = threshold;
                effects.push(Effect::ApplyChargeThreshold(threshold));
                effects.push(Effect::PersistState);
                effects.push(Effect::EmitDbusPropertiesChanged);
            }
        }

        Transition::ConfigReload => {
            effects.push(Effect::EmitDbusPropertiesChanged);
        }

        Transition::SyncPowerTarget(real_target) => {
            // A forzed rollback. Kernel overrides (or stop) the hdp config.
            new_state.power_target = real_target;
            // Emit the real values to UI, instead of value that hdp tried to set 
            effects.push(Effect::EmitDbusPropertiesChanged);
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
        effects.push(Effect::EmitDbusPropertiesChanged);

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
            is_ac_connected: true,
            charge_end_threshold: 80,
            fan_follows_tdp: true,
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

        assert!(result.is_err(), "Debería fallar porque SPPT < SPL");
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
}