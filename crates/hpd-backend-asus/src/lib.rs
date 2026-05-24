// SPDX-License-Identifier: GPL-3.0-or-later

pub mod charge;
pub mod detect;
pub mod fan;
pub mod power;
pub mod profile;

use hpd_capabilities::backend::HwBackend;
use hpd_capabilities::charge::ChargeControl;
use hpd_capabilities::error::HpdError;
use hpd_capabilities::fan::FanControl;
use hpd_capabilities::platform_profile::PlatformProfile;
use hpd_capabilities::power::{PowerEnvelope, PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_capabilities::profile::ProfileName;
use hpd_capabilities::units::Rpm;
use hpd_sysfs::SysfsIo;

pub struct AsusBackend<S: SysfsIo + Clone> {
    pub power: power::AsusPowerBackend<S>,
    pub charge: charge::AsusChargeBackend<S>,
    pub fan: fan::AsusFanBackend<S>,
    pub profile: profile::AsusProfileBackend<S>,
}

impl<S: SysfsIo + Clone> AsusBackend<S> {
    pub fn new(sysfs: S) -> Self {
        Self {
            power: power::AsusPowerBackend::new(sysfs.clone()),
            charge: charge::AsusChargeBackend::new(sysfs.clone()),
            fan: fan::AsusFanBackend::new(sysfs.clone()),
            profile: profile::AsusProfileBackend::new(sysfs),
        }
    }
}

impl<S: SysfsIo + Clone> PowerEnvelope for AsusBackend<S> {
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError> {
        self.power.get_limits()
    }
    fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError> {
        self.power.get_target()
    }
    fn set_target(&self, target: &PowerEnvelopeTarget) -> Result<(), HpdError> {
        self.power.set_target(target)
    }
}

impl<S: SysfsIo + Clone> ChargeControl for AsusBackend<S> {
    fn is_ac_connected(&self) -> Result<bool, HpdError> {
        self.charge.is_ac_connected()
    }
    fn set_end_threshold(&self, threshold: u8) -> Result<(), HpdError> {
        self.charge.set_end_threshold(threshold)
    }
    fn get_end_threshold(&self) -> Result<u8, HpdError> {
        self.charge.get_end_threshold()
    }
}

impl<S: SysfsIo + Clone> FanControl for AsusBackend<S> {
    fn get_cpu_fan_rpm(&self) -> Result<Rpm, HpdError> {
        self.fan.get_cpu_fan_rpm()
    }
    fn get_gpu_fan_rpm(&self) -> Result<Option<Rpm>, HpdError> {
        self.fan.get_gpu_fan_rpm()
    }
}

impl<S: SysfsIo + Clone> PlatformProfile for AsusBackend<S> {
    fn get_active_profile(&self) -> Result<ProfileName, HpdError> {
        self.profile.get_active_profile()
    }
    fn set_active_profile(&self, profile: &ProfileName) -> Result<(), HpdError> {
        self.profile.set_active_profile(profile)
    }
}

impl<S: SysfsIo + Clone> HwBackend for AsusBackend<S> {}
