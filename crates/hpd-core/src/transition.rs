use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::ProfileName;

/// Represents any external event who is trying to alter the state
#[derive(Debug, Clone)]
pub enum Transition {
    SetSpl(u32),
    SetEnvelope(PowerEnvelopeTarget),
    SetProfile(ProfileName),
    AcChanged(bool),
    ChargeThresholdChanged(u8),
    ConfigReload,
    SyncPowerTarget(PowerEnvelopeTarget),
}