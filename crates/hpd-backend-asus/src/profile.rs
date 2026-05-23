use hpd_capabilities::error::HpdError;
use hpd_capabilities::platform_profile::PlatformProfile;
use hpd_capabilities::profile::ProfileName;
use hpd_sysfs::SysfsIo;

const PROFILE_PATH: &str = "/sys/firmware/acpi/platform_profile";
const CHOICES_PATH: &str = "/sys/firmware/acpi/platform_profile_choices";

const ACPI_QUIET: &str = "quiet";
const ACPI_LOW_POWER: &str = "low-power";
const ACPI_BALANCED: &str = "balanced";
const ACPI_PERFORMANCE: &str = "performance";

pub struct AsusProfileBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> AsusProfileBackend<S> {
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }

    /// Read the `_choices` file and return the real options exposed by the kernel.
    fn get_available_choices(&self) -> Result<Vec<String>, HpdError> {
        let val_str = self.sysfs.read_string(CHOICES_PATH)?;
        Ok(val_str.split_whitespace().map(|s| s.to_string()).collect())
    }

    /// Parser from kernel to abstract domain
    fn parse_profile(val: &str) -> ProfileName {
        match val {
            ACPI_LOW_POWER | ACPI_QUIET => ProfileName::PowerSaver,
            ACPI_BALANCED => ProfileName::Balanced,
            ACPI_PERFORMANCE => ProfileName::Performance,
            other => ProfileName::Custom(other.to_string()),
        }
    }

    /// Map our abstract domain to a kernel-supported string, using the available options.
    fn resolve_target_string(&self, profile: &ProfileName, choices: &[String]) -> String {
        match profile {
            ProfileName::PowerSaver => {
                if choices.contains(&ACPI_QUIET.to_string()) {
                    ACPI_QUIET.to_string()
                } else if choices.contains(&ACPI_LOW_POWER.to_string()) {
                    ACPI_LOW_POWER.to_string()
                } else {
                    ACPI_BALANCED.to_string() // Safe fallback
                }
            }
            ProfileName::Balanced => ACPI_BALANCED.to_string(), // Universal on x86
            ProfileName::Performance => ACPI_PERFORMANCE.to_string(),
            ProfileName::Custom(c) => c.clone(),
        }
    }
}

impl<S: SysfsIo> PlatformProfile for AsusProfileBackend<S> {
    fn get_active_profile(&self) -> Result<ProfileName, HpdError> {
        let val_str = self.sysfs.read_string(PROFILE_PATH)?;
        Ok(Self::parse_profile(&val_str))
    }

    fn set_active_profile(&self, profile: &ProfileName) -> Result<(), HpdError> {
        let choices = self.get_available_choices()?;
        let target_str = self.resolve_target_string(profile, &choices);
        self.sysfs.write_string(PROFILE_PATH, &target_str)?;
        Ok(())
    }
}
