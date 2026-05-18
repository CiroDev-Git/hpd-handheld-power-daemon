use hpd_capabilities::power::{PowerEnvelope, PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_capabilities::error::HpdError;
use hpd_sysfs::SysfsIo;

pub struct AsusPowerBackend<S: SysfsIo> {
    sysfs: S,
    // Cached paths based on armoury
}

impl<S: SysfsIo> PowerEnvelope for AsusPowerBackend<S> {
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError> {
        todo!()
    }
    
    fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError> {
        todo!()
    }
    
    fn set_target(&self, _target: &PowerEnvelopeTarget) -> Result<(), HpdError> {
        todo!()
    }
}