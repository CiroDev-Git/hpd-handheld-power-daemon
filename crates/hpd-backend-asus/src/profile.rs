use hpd_capabilities::error::HpdError;
use hpd_capabilities::platform_profile::PlatformProfile;
use hpd_capabilities::profile::ProfileName;
use hpd_sysfs::SysfsIo;

const PROFILE_PATH: &str = "/sys/firmware/acpi/platform_profile";

pub struct AsusProfileBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> AsusProfileBackend<S> {
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }

    fn parse_profile(val: &str) -> ProfileName {
        match val {
            "low-power" | "quiet" => ProfileName::PowerSaver,
            "balanced" => ProfileName::Balanced,
            "performance" => ProfileName::Performance,
            other => ProfileName::Custom(other.to_string()),
        }
    }

    fn profile_to_str(profile: &ProfileName) -> &'static str {
        match profile {
            ProfileName::PowerSaver => "quiet", // ASUS use 'quiet' or 'low-power'
            ProfileName::Balanced => "balanced",
            ProfileName::Performance => "performance",
            ProfileName::Custom(_) => "balanced", // Safe fallback
        }
    }
}

impl<S: SysfsIo> PlatformProfile for AsusProfileBackend<S> {
    fn get_active_profile(&self) -> Result<ProfileName, HpdError> {
        let val_str = self.sysfs.read_string(PROFILE_PATH).map_err(|e| HpdError::Backend {
            reason: format!("Failed to read platform profile: {}", e)
        })?;
        
        Ok(Self::parse_profile(&val_str))
    }

    fn set_active_profile(&self, profile: &ProfileName) -> Result<(), HpdError> {
        let val_str = Self::profile_to_str(profile);
        self.sysfs.write_string(PROFILE_PATH, val_str).map_err(|e| HpdError::Backend {
            reason: format!("Failed to set platform profile: {}", e)
        })?;
        
        Ok(())
    }
}