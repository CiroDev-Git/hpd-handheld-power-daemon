use hpd_capabilities::error::HpdError;
use hpd_capabilities::fan::FanControl;
use hpd_capabilities::units::Rpm;
use hpd_sysfs::SysfsIo;

pub struct AsusFanBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> AsusFanBackend<S> {
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }

    /// Search in what dynamic folder of hwmon are the ASUS fans
    fn find_fan_path(&self, fan_index: u8) -> Result<String, HpdError> {
        for i in 0..10 {
            let path = format!("/sys/class/hwmon/hwmon{}/fan{}_input", i, fan_index);
            if self.sysfs.exists(&path) {
                return Ok(path);
            }
        }
        Err(HpdError::FeatureUnsupported)
    }

    fn read_rpm(&self, fan_index: u8) -> Result<Rpm, HpdError> {
        let path = self.find_fan_path(fan_index)?;
        let val_str = self
            .sysfs
            .read_string(&path)
            .map_err(|e| HpdError::Backend {
                reason: format!("Failed to read fan {}: {}", fan_index, e),
            })?;

        let rpm: u16 = val_str.parse().map_err(|_| HpdError::Backend {
            reason: format!("Fan {} RPM is not a valid number", fan_index),
        })?;

        Ok(Rpm(rpm))
    }
}

impl<S: SysfsIo> FanControl for AsusFanBackend<S> {
    fn get_cpu_fan_rpm(&self) -> Result<Rpm, HpdError> {
        self.read_rpm(1) // fan1_input
    }

    fn get_gpu_fan_rpm(&self) -> Result<Option<Rpm>, HpdError> {
        // Ally X: fan2_input is the GPU.
        // If the input file doesn't exist (e.g., other ASUS models), return Ok(None).
        match self.read_rpm(2) {
            Ok(rpm) => Ok(Some(rpm)),
            Err(HpdError::FeatureUnsupported) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
