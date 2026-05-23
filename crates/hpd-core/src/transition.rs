use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::{ProfileName, TdpPreset};

/// Represents any external event who is trying to alter the state
#[derive(Debug, Clone)]
pub enum Transition {
    SetPreset(TdpPreset),
    SetSpl(u32),
    SetEnvelope(PowerEnvelopeTarget),
    SetProfile(ProfileName),
    ChargeThresholdChanged(u8),
    SyncPowerTarget(PowerEnvelopeTarget),
    AcPowerChanged(bool),
    SystemResumed,
    EnableFanAuto
}