use hpd_capabilities::error::HpdError;
use hpd_capabilities::power::{PowerEnvelope, PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_sysfs::SysfsIo;

pub struct LenovoPowerBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> LenovoPowerBackend<S> {
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }
}

impl<S: SysfsIo> PowerEnvelope for LenovoPowerBackend<S> {
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError> {
        Err(HpdError::Backend { 
            reason: "Lenovo limits implementation pending. See docs/devices/lenovo.md".into() 
        })
    }

    fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError> {
        Err(HpdError::FeatureUnsupported)
    }

    fn set_target(&self, _target: &PowerEnvelopeTarget) -> Result<(), HpdError> {
        Err(HpdError::FeatureUnsupported)
    }
}