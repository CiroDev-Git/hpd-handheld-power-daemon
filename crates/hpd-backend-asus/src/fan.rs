use hpd_capabilities::error::{BackendError, HpdError};
use hpd_capabilities::fan::FanControl;
use hpd_capabilities::units::Rpm;
use hpd_sysfs::SysfsIo;

/// Upper bound (exclusive) on hwmon device indices the kernel may assign.
/// We probe `/sys/class/hwmon/hwmon{0..MAX_HWMON_INDEX}` because the
/// hwmon registration order is not stable across boots or driver loads.
const MAX_HWMON_INDEX: u8 = 10;

/// hwmon `fanN_input` index assignment for ASUS handhelds (Ally / Ally X).
/// `fan1` is the CPU/SoC fan; `fan2` (Ally X only) is the GPU/dGPU fan.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
enum FanIndex {
    Cpu = 1,
    Gpu = 2,
}

pub struct AsusFanBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> AsusFanBackend<S> {
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }

    /// Search in what dynamic folder of hwmon are the ASUS fans
    fn find_fan_path(&self, fan: FanIndex) -> Result<String, HpdError> {
        for i in 0..MAX_HWMON_INDEX {
            let path = format!("/sys/class/hwmon/hwmon{}/fan{}_input", i, fan as u8);
            if self.sysfs.exists(&path) {
                return Ok(path);
            }
        }
        Err(HpdError::FeatureUnsupported)
    }

    fn read_rpm(&self, fan: FanIndex) -> Result<Rpm, HpdError> {
        let path = self.find_fan_path(fan)?;
        let val_str = self.sysfs.read_string(&path)?;
        let rpm: u16 = val_str.parse().map_err(|_| BackendError::ParseFailed {
            field: "fan_rpm",
            raw: val_str.clone(),
            reason: format!("fan{} RPM is not a valid u16", fan as u8),
        })?;
        Ok(Rpm(rpm))
    }
}

impl<S: SysfsIo> FanControl for AsusFanBackend<S> {
    fn get_cpu_fan_rpm(&self) -> Result<Rpm, HpdError> {
        self.read_rpm(FanIndex::Cpu)
    }

    fn get_gpu_fan_rpm(&self) -> Result<Option<Rpm>, HpdError> {
        // Ally X exposes fan2_input as the GPU fan; other models don't.
        match self.read_rpm(FanIndex::Gpu) {
            Ok(rpm) => Ok(Some(rpm)),
            Err(HpdError::FeatureUnsupported) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
