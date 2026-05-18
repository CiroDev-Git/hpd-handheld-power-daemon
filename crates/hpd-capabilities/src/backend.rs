use crate::charge::ChargeControl;
use crate::fan::FanControl;
use crate::platform_profile::PlatformProfile;
use crate::power::PowerEnvelope;

/// The 'supreme' trait who knows all capabilities. 
/// Each Backend of L1 (Asus, Lenovo, Valve) should implement it
pub trait HwBackend: PowerEnvelope + ChargeControl + PlatformProfile + FanControl + Send + Sync {}