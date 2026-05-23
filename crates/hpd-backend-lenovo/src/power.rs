use hpd_capabilities::error::{BackendError, HpdError};
use hpd_capabilities::power::{PowerEnvelope, PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_sysfs::SysfsIo;

pub struct LenovoPowerBackend<S: SysfsIo> {
    _sysfs: S,
}

impl<S: SysfsIo> LenovoPowerBackend<S> {
    pub fn new(_sysfs: S) -> Self {
        Self { _sysfs }
    }
}

impl<S: SysfsIo> PowerEnvelope for LenovoPowerBackend<S> {
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError> {
        Err(BackendError::Other(
            "Lenovo limits implementation pending. See docs/devices/lenovo.md".into(),
        )
        .into())
    }

    fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError> {
        Err(HpdError::FeatureUnsupported)
    }

    fn set_target(&self, _target: &PowerEnvelopeTarget) -> Result<(), HpdError> {
        Err(HpdError::FeatureUnsupported)
    }
}
