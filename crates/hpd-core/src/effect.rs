use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::ProfileName;

/// Represents an action with side effect (I/O) that Executor should dispatch
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    ApplyPowerEnvelope(PowerEnvelopeTarget),
    ApplyPlatformProfile(ProfileName),
    ApplyChargeThreshold(u8),
    PersistState,
    EmitDbusPropertiesChanged,
}