use hpd_capabilities::charge::{ChargeControl, MAX_CHARGE_THRESHOLD, MIN_CHARGE_THRESHOLD};
use hpd_capabilities::error::{BackendError, HpdError};
use hpd_sysfs::SysfsIo;

const BATTERY_PATH: &str = "/sys/class/power_supply/BAT0";
const AC_PATHS: [&str; 4] = [
    "/sys/class/power_supply/AC/online",
    "/sys/class/power_supply/ACAD/online",
    "/sys/class/power_supply/ADP0/online",
    "/sys/class/power_supply/ADP1/online",
];

pub struct AsusChargeBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> AsusChargeBackend<S> {
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }
}

impl<S: SysfsIo> ChargeControl for AsusChargeBackend<S> {
    fn is_ac_connected(&self) -> Result<bool, HpdError> {
        for path in AC_PATHS.iter() {
            if let Ok(val_str) = self.sysfs.read_string(path) {
                return Ok(val_str.trim() == "1");
            }
        }

        // Fail-Safe
        Ok(false)
    }

    fn get_end_threshold(&self) -> Result<u8, HpdError> {
        let path = format!("{}/charge_control_end_threshold", BATTERY_PATH);
        let val_str = self.sysfs.read_string(&path)?;
        let threshold: u8 = val_str.parse().map_err(|_| BackendError::ParseFailed {
            field: "charge_end_threshold",
            raw: val_str.clone(),
            reason: "expected u8 (0-100)".into(),
        })?;
        Ok(threshold)
    }

    fn set_end_threshold(&self, threshold: u8) -> Result<(), HpdError> {
        if !(MIN_CHARGE_THRESHOLD..=MAX_CHARGE_THRESHOLD).contains(&threshold) {
            return Err(HpdError::InvariantViolation(format!(
                "charge threshold must be between {} and {}, got {}",
                MIN_CHARGE_THRESHOLD, MAX_CHARGE_THRESHOLD, threshold
            )));
        }

        let path = format!("{}/charge_control_end_threshold", BATTERY_PATH);
        self.sysfs.write_string(&path, &threshold.to_string())?;
        Ok(())
    }
}
