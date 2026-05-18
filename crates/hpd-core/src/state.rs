use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::ProfileName;

/// Immutable state of hpd.
/// L3 Executor keeps this inside of Arc<RwLock<ProfileState>>.
#[derive(Debug, Clone, PartialEq)]
pub struct ProfileState {
    pub power_target: PowerEnvelopeTarget,
    pub active_profile: ProfileName,
    pub is_ac_connected: bool,
    pub charge_end_threshold: u8,
    pub fan_follows_tdp: bool,
}