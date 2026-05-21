use crate::error::HpdError;
use crate::units::PowerMilliwatts;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PowerEnvelopeLimits {
    pub spl_min: PowerMilliwatts,
    pub spl_max: PowerMilliwatts,
    pub sppt_max: PowerMilliwatts,
    pub fppt_max: PowerMilliwatts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PowerEnvelopeTarget {
    pub spl: PowerMilliwatts,
    pub sppt: PowerMilliwatts,
    pub fppt: Option<PowerMilliwatts>, // For e.x Lenovo doesn't have it
}

pub trait PowerEnvelope: Send + Sync {
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError>;
    
    fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError>;
    
    /// L3 must validate FPPT ≥ SPPT ≥ SPL before call this function
    fn set_target(&self, target: &PowerEnvelopeTarget) -> Result<(), HpdError>;
}