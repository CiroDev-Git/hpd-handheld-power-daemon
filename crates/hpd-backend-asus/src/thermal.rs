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
//! Both are located by hwmon `name` (indices are not stable; see the
//! crate's `hwmon` module) and read in millidegrees, divided down to
//! whole degrees Celsius.

use hpd_capabilities::thermal::ThermalSensors;
use hpd_capabilities::units::{Celsius, PowerMilliwatts};
use hpd_error::{BackendError, HpdError};
use hpd_sysfs::SysfsIo;

use crate::hwmon::HwmonCache;

/// hwmon `name` of the AMD CPU temperature driver (Tctl).
const CPU_HWMON_NAME: &str = "k10temp";
/// hwmon `name` of the AMD GPU temperature driver (edge).
const GPU_HWMON_NAME: &str = "amdgpu";

/// [`ThermalSensors`] implementation for ASUS AMD handhelds.
pub struct AsusThermalBackend<S: SysfsIo> {
    sysfs: S,
    hwmon_cache: HwmonCache,
}

impl<S: SysfsIo> AsusThermalBackend<S> {
    /// Wrap a `SysfsIo` handle (see [`AsusBackend::new`](crate::AsusBackend::new)).
    pub fn new(sysfs: S) -> Self {
        Self {
            sysfs,
            hwmon_cache: HwmonCache::new(),
        }
    }

    /// Read `temp1_input` (millidegrees) under the hwmon named `name`,
    /// returning `Ok(None)` when that sensor node is absent.
    fn read_temp(&self, name: &'static str) -> Result<Option<Celsius>, HpdError> {
        let Some(raw) = self
            .hwmon_cache
            .read_attr(&self.sysfs, name, "temp1_input")?
        else {
            return Ok(None);
        };
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

    /// Read `power1_input` (microwatts) under the `amdgpu` hwmon. On the
    /// AMD APU this reports the SoC/package power the SMU is actually
    /// drawing — a good live proxy for "how hard the chip is working"
    /// next to the configured TDP limit. `Ok(None)` if absent.
    fn get_soc_power(&self) -> Result<Option<PowerMilliwatts>, HpdError> {
        let Some(raw) = self
            .hwmon_cache
            .read_attr(&self.sysfs, GPU_HWMON_NAME, "power1_input")?
        else {
            return Ok(None);
        };
        let micro: u64 = raw.trim().parse().map_err(|_| BackendError::ParseFailed {
            field: "power1_input",
            raw: raw.clone(),
            reason: "expected integer microwatts".into(),
        })?;
        // microwatts → milliwatts; clamp to u32 (the chip never pulls
        // anywhere near 4.29 kW, so this only guards a garbage read).
        let milli = (micro / 1000).min(u64::from(u32::MAX)) as u32;
        Ok(Some(PowerMilliwatts(milli)))
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
        assert_eq!(backend.get_soc_power().unwrap(), None);
    }

    #[test]
    fn reads_soc_power_from_amdgpu_microwatts() {
        let mock = MockSysfs::new();
        mock.create_file("sys/class/hwmon/hwmon5/name", "amdgpu");
        mock.create_file("sys/class/hwmon/hwmon5/temp1_input", "72000");
        mock.create_file("sys/class/hwmon/hwmon5/power1_input", "16088000"); // 16.088 W
        let backend = AsusThermalBackend::new(mock.clone());
        // microwatts → milliwatts
        assert_eq!(
            backend.get_soc_power().unwrap(),
            Some(PowerMilliwatts(16088))
        );
    }

    #[test]
    fn soc_power_none_when_node_lacks_power_input() {
        let mock = MockSysfs::new();
        mock.create_file("sys/class/hwmon/hwmon5/name", "amdgpu");
        mock.create_file("sys/class/hwmon/hwmon5/temp1_input", "72000");
        // No power1_input on this node.
        let backend = AsusThermalBackend::new(mock.clone());
        assert_eq!(backend.get_soc_power().unwrap(), None);
    }
}
