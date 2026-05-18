use crate::error::HpdError;

pub trait ChargeControl: Send + Sync {
    fn set_end_threshold(&self, threshold: u8) -> Result<(), HpdError>;
    
    fn get_end_threshold(&self) -> Result<u8, HpdError>;
}