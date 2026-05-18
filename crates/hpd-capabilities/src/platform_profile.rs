use crate::error::HpdError;
use crate::profile::ProfileName;

pub trait PlatformProfile: Send + Sync {
    fn get_active_profile(&self) -> Result<ProfileName, HpdError>;
    fn set_active_profile(&self, profile: &ProfileName) -> Result<(), HpdError>;
}