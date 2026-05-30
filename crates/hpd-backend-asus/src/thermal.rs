// SPDX-License-Identifier: GPL-3.0-or-later

//! [`ThermalSensors`] implementation for ASUS AMD handhelds.
//!
//! The Ally family is built on an AMD APU, so the principal
//! temperatures come from the generic AMD drivers rather than an ASUS
//! node:
//!
//! * **CPU/SoC** — `k10temp` hwmon, `temp1_input` (Tctl).
//! * **GPU** — `amdgpu` hwmon, `temp1_input` (edge).
//!
//! Both are located by hwmon `name` (indices are not stable; see
//! [`crate::hwmon`]) and read in millidegrees, divided down to whole
//! degrees Celsius.

use hpd_capabilities::thermal::ThermalSensors;
use hpd_capabilities::units::Celsius;
use hpd_error::{BackendError, HpdError};
use hpd_sysfs::SysfsIo;

use crate::hwmon::find_hwmon_by_name;

/// hwmon `name` of the AMD CPU temperature driver (Tctl).
const CPU_HWMON_NAME: &str = "k10temp";
/// hwmon `name` of the AMD GPU temperature driver (edge).
const GPU_HWMON_NAME: &str = "amdgpu";

/// [`ThermalSensors`] implementation for ASUS AMD handhelds.
pub struct AsusThermalBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> AsusThermalBackend<S> {
    /// Wrap a `SysfsIo` handle (see [`AsusBackend::new`](crate::AsusBackend::new)).
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }

    /// Read `temp1_input` (millidegrees) under the hwmon named `name`,
    /// returning `Ok(None)` when that sensor node is absent.
    fn read_temp(&self, name: &str) -> Result<Option<Celsius>, HpdError> {
        let Some(base) = find_hwmon_by_name(&self.sysfs, name) else {
            return Ok(None);
        };
        let path = format!("{base}/temp1_input");
        let raw = self.sysfs.read_string(&path)?;
        let milli: i32 = raw.trim().parse().map_err(|_| BackendError::ParseFailed {
            field: "temp_input",
            raw: raw.clone(),
            reason: "expected integer millidegrees".into(),
        })?;
        Ok(Some(Celsius::from_millidegrees(milli)))
    }
}

impl<S: SysfsIo> ThermalSensors for AsusThermalBackend<S> {
    fn get_cpu_temp(&self) -> Result<Option<Celsius>, HpdError> {
        self.read_temp(CPU_HWMON_NAME)
    }

    fn get_gpu_temp(&self) -> Result<Option<Celsius>, HpdError> {
        self.read_temp(GPU_HWMON_NAME)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_sysfs::MockSysfs;

    #[test]
    fn reads_cpu_and_gpu_temps_by_name() {
        let mock = MockSysfs::new();
        mock.create_file("sys/class/hwmon/hwmon6/name", "k10temp");
        mock.create_file("sys/class/hwmon/hwmon6/temp1_input", "87125"); // 87.125 °C
        mock.create_file("sys/class/hwmon/hwmon5/name", "amdgpu");
        mock.create_file("sys/class/hwmon/hwmon5/temp1_input", "72000");

        let backend = AsusThermalBackend::new(mock.clone());
        assert_eq!(backend.get_cpu_temp().unwrap(), Some(Celsius(87)));
        assert_eq!(backend.get_gpu_temp().unwrap(), Some(Celsius(72)));
    }

    #[test]
    fn absent_sensor_reports_none() {
        let mock = MockSysfs::new();
        mock.create_file("sys/class/hwmon/hwmon6/name", "k10temp");
        mock.create_file("sys/class/hwmon/hwmon6/temp1_input", "60000");
        // No amdgpu node seeded.
        let backend = AsusThermalBackend::new(mock.clone());
        assert_eq!(backend.get_cpu_temp().unwrap(), Some(Celsius(60)));
        assert_eq!(backend.get_gpu_temp().unwrap(), None);
    }
}
