use crate::error::HpdError;

pub const MIN_CHARGE_THRESHOLD: u8 = 20;
pub const MAX_CHARGE_THRESHOLD: u8 = 100;

pub trait ChargeControl: Send + Sync {
    fn is_ac_connected(&self) -> Result<bool, HpdError>;

    fn set_end_threshold(&self, threshold: u8) -> Result<(), HpdError>;
    
    fn get_end_threshold(&self) -> Result<u8, HpdError>;
}