use crate::power::PowerEnvelope;
use crate::charge::ChargeControl;
use crate::platform_profile::PlatformProfile;
// FanControl here soon

pub trait HwBackend: PowerEnvelope + ChargeControl + PlatformProfile + Send + Sync {}