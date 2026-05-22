use serde::{Deserialize, Serialize};
use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::ProfileName;

/// Immutable state of hpd.
/// L3 Executor keeps this inside of Arc<RwLock<ProfileState>>.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileState {
    pub power_target: PowerEnvelopeTarget,
    pub active_profile: ProfileName,
    pub charge_end_threshold: u8,
    pub fan_follows_tdp: bool,
    pub last_dc_target: Option<PowerEnvelopeTarget>,
    
    // Ignore is_ac_connected in storage since at reboot time 
    // we ask to hardware again about if it's charging or not
    #[serde(skip)] 
    pub is_ac_connected: bool,
}