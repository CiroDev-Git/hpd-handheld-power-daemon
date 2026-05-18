use hpd_capabilities::error::HpdError;
use hpd_capabilities::power::PowerEnvelopeLimits;
use hpd_capabilities::profile::ProfileThresholds;

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
        Transition::SetEnvelope(new_target) => {
            validate_power_envelope(&new_target)?;

            if new_state.power_target != new_target {
                new_state.power_target = new_target.clone();
                effects.push(Effect::ApplyPowerEnvelope(new_target.clone()));

                if new_state.fan_follows_tdp {
                    let inferred_profile = infer_profile_from_spl(
                        &new_target,
                        device_limits,
                        profile_thresholds,
                    );

                    if new_state.active_profile != inferred_profile {
                        new_state.active_profile = inferred_profile.clone();
                        effects.push(Effect::ApplyPlatformProfile(inferred_profile));
                    }
                }

                effects.push(Effect::PersistState);
                effects.push(Effect::EmitDbusPropertiesChanged);
            }
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
    }

    Ok(ReducerOutput { new_state, effects })
}