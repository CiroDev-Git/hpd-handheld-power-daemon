// SPDX-License-Identifier: GPL-3.0-or-later

use hpd_capabilities::fan::FanControl;
use hpd_capabilities::units::Rpm;
use hpd_error::{BackendError, HpdError};
use hpd_sysfs::SysfsIo;

use crate::hwmon::find_hwmon_by_name;

/// hwmon `name` of the ASUS sensor node that exposes `fanN_input`.
/// Distinct from `asus_custom_fan_curve` (the writable curve node) and
/// from `acpi_fan` (a generic node that also exposes a `fan1_input` and
/// would shadow this one under a naive lowest-index scan).
const ASUS_FAN_HWMON_NAME: &str = "asus";

/// hwmon `fanN_input` index assignment for ASUS handhelds (Ally / Ally X).
/// `fan1` is the CPU/SoC fan; `fan2` (Ally X only) is the GPU/dGPU fan.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
enum FanIndex {
    Cpu = 1,
    Gpu = 2,
}

/// [`FanControl`] implementation for ASUS handhelds.
///
/// Locates the `asus` hwmon node by its `name` attribute (not by index —
/// the kernel does not guarantee a stable hwmon registration order), then
/// reads `fanN_input` under it.
pub struct AsusFanBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> AsusFanBackend<S> {
    /// Wrap a `SysfsIo` handle (see [`AsusBackend::new`](crate::AsusBackend::new)).
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }

    /// Resolve the `fanN_input` path under the `asus` hwmon node.
    /// Returns [`HpdError::FeatureUnsupported`] when the node is absent
    /// (non-ASUS hardware) or the requested fan index is not exposed
    /// (single-fan models lack `fan2_input`).
    fn find_fan_path(&self, fan: FanIndex) -> Result<String, HpdError> {
        let base = find_hwmon_by_name(&self.sysfs, ASUS_FAN_HWMON_NAME)
            .ok_or(HpdError::FeatureUnsupported)?;
        let path = format!("{}/fan{}_input", base, fan as u8);
        if self.sysfs.exists(&path) {
            Ok(path)
        } else {
            Err(HpdError::FeatureUnsupported)
        }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_sysfs::MockSysfs;

    /// Regression for the lowest-index hwmon scan bug: on the Xbox Ally X
    /// `acpi_fan` (a low index) also exposes `fan1_input`, which the old
    /// scan would read instead of the real `asus` node. We must resolve
    /// `asus` by name and read its RPM, not the `acpi_fan` shadow.
    #[test]
    fn reads_asus_node_not_acpi_fan_shadow() {
        let mock = MockSysfs::new();
        mock.create_file("sys/class/hwmon/hwmon1/name", "acpi_fan");
        mock.create_file("sys/class/hwmon/hwmon1/fan1_input", "9999"); // decoy
        mock.create_file("sys/class/hwmon/hwmon7/name", "asus");
        mock.create_file("sys/class/hwmon/hwmon7/fan1_input", "6400");
        mock.create_file("sys/class/hwmon/hwmon7/fan2_input", "6500");

        let backend = AsusFanBackend::new(mock.clone());
        assert_eq!(backend.get_cpu_fan_rpm().unwrap(), Rpm(6400));
        assert_eq!(backend.get_gpu_fan_rpm().unwrap(), Some(Rpm(6500)));
    }

    #[test]
    fn single_fan_model_reports_no_gpu_fan() {
        let mock = MockSysfs::new();
        mock.create_file("sys/class/hwmon/hwmon3/name", "asus");
        mock.create_file("sys/class/hwmon/hwmon3/fan1_input", "4200");

        let backend = AsusFanBackend::new(mock.clone());
        assert_eq!(backend.get_cpu_fan_rpm().unwrap(), Rpm(4200));
        assert_eq!(backend.get_gpu_fan_rpm().unwrap(), None);
    }
}
