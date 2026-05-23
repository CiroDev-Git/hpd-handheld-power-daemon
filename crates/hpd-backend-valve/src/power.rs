use hpd_capabilities::error::{BackendError, HpdError};
use hpd_capabilities::power::{PowerEnvelope, PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_sysfs::SysfsIo;

pub struct ValvePowerBackend<S: SysfsIo> {
    _sysfs: S,
}

impl<S: SysfsIo> ValvePowerBackend<S> {
    pub fn new(_sysfs: S) -> Self {
        Self { _sysfs }
    }
}

impl<S: SysfsIo> PowerEnvelope for ValvePowerBackend<S> {
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError> {
        Err(BackendError::Other(
            "Valve limits implementation pending. See docs/devices/valve.md".into(),
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