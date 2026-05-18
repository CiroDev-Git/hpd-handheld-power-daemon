use crate::error::HpdError;
use crate::units::Rpm;

pub trait FanControl: Send + Sync {
    fn get_cpu_fan_rpm(&self) -> Result<Rpm, HpdError>;
    fn get_gpu_fan_rpm(&self) -> Result<Option<Rpm>, HpdError>;
}