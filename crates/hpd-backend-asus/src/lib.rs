pub mod detect;
pub mod power;

use hpd_capabilities::backend::HwBackend;
use hpd_capabilities::charge::ChargeControl;
use hpd_capabilities::fan::FanControl;
use hpd_capabilities::platform_profile::PlatformProfile;
use hpd_capabilities::power::{PowerEnvelope, PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_capabilities::error::HpdError;
use hpd_capabilities::profile::ProfileName;
use hpd_capabilities::units::Rpm;
use hpd_sysfs::SysfsIo;

pub struct AsusBackend<S: SysfsIo> {
    pub power: power::AsusPowerBackend<S>,
}

impl<S: SysfsIo> AsusBackend<S> {
    pub fn new(sysfs: S) -> Self {
        Self { power: power::AsusPowerBackend::new(sysfs) }
    }
}

impl<S: SysfsIo> PowerEnvelope for AsusBackend<S> {
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError> { self.power.get_limits() }
    fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError> { self.power.get_target() }
    fn set_target(&self, target: &PowerEnvelopeTarget) -> Result<(), HpdError> { self.power.set_target(target) }
}

impl<S: SysfsIo> ChargeControl for AsusBackend<S> {
    fn set_end_threshold(&self, _threshold: u8) -> Result<(), HpdError> { Ok(()) }
    fn get_end_threshold(&self) -> Result<u8, HpdError> { Ok(80) }
}

impl<S: SysfsIo> FanControl for AsusBackend<S> {
    fn get_cpu_fan_rpm(&self) -> Result<Rpm, HpdError> { Ok(Rpm(0)) }
    fn get_gpu_fan_rpm(&self) -> Result<Option<Rpm>, HpdError> { Ok(None) }
}

impl<S: SysfsIo> PlatformProfile for AsusBackend<S> {
    fn get_active_profile(&self) -> Result<ProfileName, HpdError> { Ok(ProfileName::Balanced) }
    fn set_active_profile(&self, _profile: &ProfileName) -> Result<(), HpdError> { Ok(()) }
}

impl<S: SysfsIo> HwBackend for AsusBackend<S> {}