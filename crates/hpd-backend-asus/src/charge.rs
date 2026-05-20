use hpd_capabilities::charge::{ChargeControl, MIN_CHARGE_THRESHOLD, MAX_CHARGE_THRESHOLD};
use hpd_capabilities::error::HpdError;
use hpd_sysfs::SysfsIo;

const BATTERY_PATH: &str = "/sys/class/power_supply/BAT0";

pub struct AsusChargeBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> AsusChargeBackend<S> {
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }
}

impl<S: SysfsIo> ChargeControl for AsusChargeBackend<S> {
    fn get_end_threshold(&self) -> Result<u8, HpdError> {
        let path = format!("{}/charge_control_end_threshold", BATTERY_PATH);
        
        let val_str = self.sysfs.read_string(&path).map_err(|e| HpdError::Backend {
            reason: format!("Failed to read battery threshold: {}", e)
        })?;

        let threshold: u8 = val_str.parse().map_err(|_| HpdError::Backend {
            reason: "Battery threshold is not a valid number".into()
        })?;

        Ok(threshold)
    }

    fn set_end_threshold(&self, threshold: u8) -> Result<(), HpdError> {
        if !(MIN_CHARGE_THRESHOLD..=MAX_CHARGE_THRESHOLD).contains(&threshold) {
            return Err(HpdError::InvariantViolation(
                format!("Charge threshold must be between 20 and 100, got {}", threshold)
            ));
        }

        let path = format!("{}/charge_control_end_threshold", BATTERY_PATH);
        
        self.sysfs.write_string(&path, &threshold.to_string()).map_err(|e| HpdError::Backend {
            reason: format!("Failed to write battery threshold: {}", e)
        })?;

        Ok(())
    }
}