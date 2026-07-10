// SPDX-License-Identifier: GPL-3.0-or-later

//! [`SystemTelemetry`] implementation for ASUS AMD handhelds.
//!
//! Battery fields come from the `power_supply` node located by
//! `type == "Battery"` (see the crate's `power_supply` module); CPU
//! frequency from the generic `cpufreq` policy tree; GPU frequency/load/
//! VRAM from the `amdgpu` hwmon node's sibling DRM device directory
//! (`{hwmon_base}/device/...`) — the same node `thermal` already locates
//! for temperature/power, resolved here through the same hwmon cache so
//! a 1 Hz telemetry poll costs one confirmation read per key instead of
//! a fresh `hwmonN` scan.

use hpd_capabilities::telemetry::SystemTelemetry;
use hpd_capabilities::units::PowerMilliwatts;
use hpd_error::{BackendError, HpdError};
use hpd_sysfs::SysfsIo;

use crate::hwmon::HwmonCache;
use crate::power_supply::find_node_by_type;

/// hwmon `name` of the AMD GPU driver — same node `thermal.rs` reads
/// temperature/power from; its DRM device sits at `{base}/device`.
const GPU_HWMON_NAME: &str = "amdgpu";
/// Root of the generic `cpufreq` policy tree.
const CPUFREQ_ROOT: &str = "/sys/devices/system/cpu/cpufreq";

/// [`SystemTelemetry`] implementation for ASUS AMD handhelds.
pub struct AsusTelemetryBackend<S: SysfsIo> {
    sysfs: S,
    hwmon_cache: HwmonCache,
}

impl<S: SysfsIo> AsusTelemetryBackend<S> {
    /// Wrap a `SysfsIo` handle (see [`AsusBackend::new`](crate::AsusBackend::new)).
    pub fn new(sysfs: S) -> Self {
        Self {
            sysfs,
            hwmon_cache: HwmonCache::new(),
        }
    }

    /// Locate the battery's `power_supply` base path, or `None` on a
    /// battery-less board.
    fn battery_base(&self) -> Option<String> {
        find_node_by_type(&self.sysfs, "Battery")
    }

    /// Read a `power_supply`-relative attribute under the battery node,
    /// `Ok(None)` if the node or the attribute is absent.
    fn read_battery_attr(&self, attr: &str) -> Result<Option<String>, HpdError> {
        let Some(base) = self.battery_base() else {
            return Ok(None);
        };
        let path = format!("{base}/{attr}");
        if !self.sysfs.exists(&path) {
            return Ok(None);
        }
        Ok(Some(self.sysfs.read_string(&path)?))
    }

    /// Read a DRM-device-relative attribute under the `amdgpu` hwmon's
    /// sibling device directory (`{hwmon_base}/device/{attr}`), `Ok(None)`
    /// if the GPU hwmon or the attribute is absent.
    fn read_gpu_device_attr(&self, attr: &str) -> Result<Option<String>, HpdError> {
        let Some(base) = self.hwmon_cache.resolve_base(&self.sysfs, GPU_HWMON_NAME) else {
            return Ok(None);
        };
        let path = format!("{base}/device/{attr}");
        if !self.sysfs.exists(&path) {
            return Ok(None);
        }
        Ok(Some(self.sysfs.read_string(&path)?))
    }

    fn read_vram_mb(&self, attr: &str, field: &'static str) -> Result<Option<u32>, HpdError> {
        let Some(raw) = self.read_gpu_device_attr(attr)? else {
            return Ok(None);
        };
        let bytes: u64 = raw.trim().parse().map_err(|_| BackendError::ParseFailed {
            field,
            raw: raw.clone(),
            reason: "expected integer bytes".into(),
        })?;
        Ok(Some((bytes / (1024 * 1024)).min(u64::from(u32::MAX)) as u32))
    }
}

impl<S: SysfsIo> SystemTelemetry for AsusTelemetryBackend<S> {
    /// Only reported while actually discharging (`status == "Discharging"`)
    /// — while charging or full, "power draw" is not the quantity a
    /// caller wants; use [`Self::get_battery_status`] for that state.
    /// Prefers the kernel's own `power_now` (µW); falls back to
    /// `current_now × voltage_now` on drivers that only expose those.
    fn get_battery_power(&self) -> Result<Option<PowerMilliwatts>, HpdError> {
        let Some(status) = self.read_battery_attr("status")? else {
            return Ok(None);
        };
        if status.trim() != "Discharging" {
            return Ok(None);
        }

        if let Some(raw) = self.read_battery_attr("power_now")? {
            let micro: u64 = raw.trim().parse().map_err(|_| BackendError::ParseFailed {
                field: "battery_power_now",
                raw: raw.clone(),
                reason: "expected integer microwatts".into(),
            })?;
            return Ok(Some(PowerMilliwatts(
                (micro / 1000).min(u64::from(u32::MAX)) as u32,
            )));
        }

        let (Some(current_raw), Some(voltage_raw)) = (
            self.read_battery_attr("current_now")?,
            self.read_battery_attr("voltage_now")?,
        ) else {
            return Ok(None);
        };
        let current_ua: u64 =
            current_raw
                .trim()
                .parse()
                .map_err(|_| BackendError::ParseFailed {
                    field: "battery_current_now",
                    raw: current_raw.clone(),
                    reason: "expected integer microamps".into(),
                })?;
        let voltage_uv: u64 =
            voltage_raw
                .trim()
                .parse()
                .map_err(|_| BackendError::ParseFailed {
                    field: "battery_voltage_now",
                    raw: voltage_raw.clone(),
                    reason: "expected integer microvolts".into(),
                })?;
        // µA × µV / 1e6 = µW; /1000 again = mW.
        let micro_watts = current_ua.saturating_mul(voltage_uv) / 1_000_000;
        Ok(Some(PowerMilliwatts(
            (micro_watts / 1000).min(u64::from(u32::MAX)) as u32,
        )))
    }

    fn get_battery_percent(&self) -> Result<Option<u8>, HpdError> {
        let Some(raw) = self.read_battery_attr("capacity")? else {
            return Ok(None);
        };
        let pct: u8 = raw.trim().parse().map_err(|_| BackendError::ParseFailed {
            field: "battery_capacity",
            raw: raw.clone(),
            reason: "expected u8 (0-100)".into(),
        })?;
        Ok(Some(pct))
    }

    fn get_battery_status(&self) -> Result<Option<String>, HpdError> {
        Ok(self
            .read_battery_attr("status")?
            .map(|s| s.trim().to_string()))
    }

    /// `charge_full * 100 / charge_full_design`, falling back to the
    /// `energy_full*` pair on drivers that report energy instead of
    /// charge. Not clamped to 100 — a freshly calibrated battery can
    /// legitimately read slightly over.
    fn get_battery_health_pct(&self) -> Result<Option<u8>, HpdError> {
        let (full_attr, design_attr): (&'static str, &'static str) =
            if self.read_battery_attr("charge_full")?.is_some() {
                ("charge_full", "charge_full_design")
            } else if self.read_battery_attr("energy_full")?.is_some() {
                ("energy_full", "energy_full_design")
            } else {
                return Ok(None);
            };
        let Some(full_raw) = self.read_battery_attr(full_attr)? else {
            return Ok(None);
        };
        let Some(design_raw) = self.read_battery_attr(design_attr)? else {
            return Ok(None);
        };
        let full: u64 = full_raw
            .trim()
            .parse()
            .map_err(|_| BackendError::ParseFailed {
                field: full_attr,
                raw: full_raw.clone(),
                reason: "expected integer".into(),
            })?;
        let design: u64 = design_raw
            .trim()
            .parse()
            .map_err(|_| BackendError::ParseFailed {
                field: design_attr,
                raw: design_raw.clone(),
                reason: "expected integer".into(),
            })?;
        if design == 0 {
            return Ok(None);
        }
        Ok(Some(
            (full.saturating_mul(100) / design).min(u64::from(u8::MAX)) as u8,
        ))
    }

    fn get_battery_cycles(&self) -> Result<Option<u32>, HpdError> {
        let Some(raw) = self.read_battery_attr("cycle_count")? else {
            return Ok(None);
        };
        let cycles: u32 = raw.trim().parse().map_err(|_| BackendError::ParseFailed {
            field: "battery_cycle_count",
            raw: raw.clone(),
            reason: "expected u32".into(),
        })?;
        Ok(Some(cycles))
    }

    /// Average of every `cpufreq/policy*/scaling_cur_freq` (kHz →
    /// truncated to whole MHz). `None` only if the whole tree is absent
    /// (no `cpufreq` sysfs at all); an individual policy missing the
    /// attribute is simply excluded from the average.
    fn get_cpu_freq_mhz(&self) -> Result<Option<u32>, HpdError> {
        let policies: Vec<String> = self
            .sysfs
            .read_dir_names(CPUFREQ_ROOT)
            .into_iter()
            .filter(|n| n.starts_with("policy"))
            .collect();
        if policies.is_empty() {
            return Ok(None);
        }
        let mut sum: u64 = 0;
        let mut count: u64 = 0;
        for policy in &policies {
            let path = format!("{CPUFREQ_ROOT}/{policy}/scaling_cur_freq");
            if !self.sysfs.exists(&path) {
                continue;
            }
            let raw = self.sysfs.read_string(&path)?;
            let khz: u64 = raw.trim().parse().map_err(|_| BackendError::ParseFailed {
                field: "scaling_cur_freq",
                raw: raw.clone(),
                reason: "expected integer kHz".into(),
            })?;
            sum += khz;
            count += 1;
        }
        if count == 0 {
            return Ok(None);
        }
        Ok(Some((sum / count / 1000) as u32))
    }

    /// Prefers the `amdgpu` hwmon's `freq1_input` (Hz, per the hwmon
    /// sysfs ABI); falls back to parsing the active (` *`-marked) line of
    /// `pp_dpm_sclk` on kernels that don't expose the hwmon frequency
    /// input.
    fn get_gpu_freq_mhz(&self) -> Result<Option<u32>, HpdError> {
        if let Some(raw) = self
            .hwmon_cache
            .read_attr(&self.sysfs, GPU_HWMON_NAME, "freq1_input")?
        {
            let hz: u64 = raw.trim().parse().map_err(|_| BackendError::ParseFailed {
                field: "gpu_freq1_input",
                raw: raw.clone(),
                reason: "expected integer Hz".into(),
            })?;
            return Ok(Some((hz / 1_000_000) as u32));
        }
        let Some(raw) = self.read_gpu_device_attr("pp_dpm_sclk")? else {
            return Ok(None);
        };
        Ok(parse_active_dpm_mhz(&raw))
    }

    fn get_gpu_busy_pct(&self) -> Result<Option<u8>, HpdError> {
        let Some(raw) = self.read_gpu_device_attr("gpu_busy_percent")? else {
            return Ok(None);
        };
        let pct: u8 = raw.trim().parse().map_err(|_| BackendError::ParseFailed {
            field: "gpu_busy_percent",
            raw: raw.clone(),
            reason: "expected u8 (0-100)".into(),
        })?;
        Ok(Some(pct))
    }

    fn get_vram_used_mb(&self) -> Result<Option<u32>, HpdError> {
        self.read_vram_mb("mem_info_vram_used", "vram_used")
    }

    fn get_vram_total_mb(&self) -> Result<Option<u32>, HpdError> {
        self.read_vram_mb("mem_info_vram_total", "vram_total")
    }

    // get_gpu_throttle_status: no known stable, non-debugfs sysfs
    // attribute for this on amdgpu as of this writing — default `None`.
}

/// Parse the amdgpu `pp_dpm_sclk` table (one `"<level>: <freq>Mhz[ *]"`
/// line per DPM level, the active one suffixed `*`) and return the
/// active level's frequency in MHz.
fn parse_active_dpm_mhz(raw: &str) -> Option<u32> {
    for line in raw.lines() {
        if !line.trim_end().ends_with('*') {
            continue;
        }
        let mhz_token = line
            .split_whitespace()
            .find(|tok| tok.to_ascii_lowercase().ends_with("mhz"))?;
        let digits: String = mhz_token
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        return digits.parse().ok();
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_sysfs::MockSysfs;

    fn seed_battery(mock: &MockSysfs) {
        mock.create_file("sys/class/power_supply/BAT0/type", "Battery");
    }

    fn seed_amdgpu(mock: &MockSysfs) {
        mock.create_file("sys/class/hwmon/hwmon5/name", "amdgpu");
    }

    #[test]
    fn battery_power_reads_power_now_while_discharging() {
        let mock = MockSysfs::new();
        seed_battery(&mock);
        mock.create_file("sys/class/power_supply/BAT0/status", "Discharging");
        mock.create_file("sys/class/power_supply/BAT0/power_now", "18300000"); // 18.3 W

        let backend = AsusTelemetryBackend::new(mock.clone());
        assert_eq!(
            backend.get_battery_power().unwrap(),
            Some(PowerMilliwatts(18300))
        );
    }

    #[test]
    fn battery_power_falls_back_to_current_times_voltage() {
        let mock = MockSysfs::new();
        seed_battery(&mock);
        mock.create_file("sys/class/power_supply/BAT0/status", "Discharging");
        // No power_now on this driver.
        mock.create_file("sys/class/power_supply/BAT0/current_now", "1000000"); // 1 A
        mock.create_file("sys/class/power_supply/BAT0/voltage_now", "15000000"); // 15 V

        let backend = AsusTelemetryBackend::new(mock.clone());
        // 1A * 15V = 15W = 15000mW
        assert_eq!(
            backend.get_battery_power().unwrap(),
            Some(PowerMilliwatts(15000))
        );
    }

    #[test]
    fn battery_power_is_none_while_charging() {
        let mock = MockSysfs::new();
        seed_battery(&mock);
        mock.create_file("sys/class/power_supply/BAT0/status", "Charging");
        mock.create_file("sys/class/power_supply/BAT0/power_now", "9000000");

        let backend = AsusTelemetryBackend::new(mock.clone());
        assert_eq!(backend.get_battery_power().unwrap(), None);
    }

    #[test]
    fn battery_fields_absent_without_a_battery_node() {
        let mock = MockSysfs::new();
        let backend = AsusTelemetryBackend::new(mock.clone());
        assert_eq!(backend.get_battery_power().unwrap(), None);
        assert_eq!(backend.get_battery_percent().unwrap(), None);
        assert_eq!(backend.get_battery_status().unwrap(), None);
        assert_eq!(backend.get_battery_health_pct().unwrap(), None);
        assert_eq!(backend.get_battery_cycles().unwrap(), None);
    }

    #[test]
    fn battery_percent_status_health_and_cycles() {
        let mock = MockSysfs::new();
        seed_battery(&mock);
        mock.create_file("sys/class/power_supply/BAT0/capacity", "64");
        mock.create_file("sys/class/power_supply/BAT0/status", "Discharging");
        mock.create_file("sys/class/power_supply/BAT0/charge_full", "87000");
        mock.create_file("sys/class/power_supply/BAT0/charge_full_design", "100000");
        mock.create_file("sys/class/power_supply/BAT0/cycle_count", "214");

        let backend = AsusTelemetryBackend::new(mock.clone());
        assert_eq!(backend.get_battery_percent().unwrap(), Some(64));
        assert_eq!(
            backend.get_battery_status().unwrap(),
            Some("Discharging".to_string())
        );
        assert_eq!(backend.get_battery_health_pct().unwrap(), Some(87));
        assert_eq!(backend.get_battery_cycles().unwrap(), Some(214));
    }

    #[test]
    fn battery_health_falls_back_to_energy_pair() {
        let mock = MockSysfs::new();
        seed_battery(&mock);
        // No charge_full* on this driver, only energy_full*.
        mock.create_file("sys/class/power_supply/BAT0/energy_full", "45000");
        mock.create_file("sys/class/power_supply/BAT0/energy_full_design", "50000");

        let backend = AsusTelemetryBackend::new(mock.clone());
        assert_eq!(backend.get_battery_health_pct().unwrap(), Some(90));
    }

    #[test]
    fn cpu_freq_averages_all_policies() {
        let mock = MockSysfs::new();
        mock.create_file(
            "sys/devices/system/cpu/cpufreq/policy0/scaling_cur_freq",
            "3200000",
        );
        mock.create_file(
            "sys/devices/system/cpu/cpufreq/policy1/scaling_cur_freq",
            "1600000",
        );

        let backend = AsusTelemetryBackend::new(mock.clone());
        // (3200000 + 1600000) / 2 = 2400000 kHz = 2400 MHz
        assert_eq!(backend.get_cpu_freq_mhz().unwrap(), Some(2400));
    }

    #[test]
    fn cpu_freq_absent_without_cpufreq_tree() {
        let mock = MockSysfs::new();
        let backend = AsusTelemetryBackend::new(mock.clone());
        assert_eq!(backend.get_cpu_freq_mhz().unwrap(), None);
    }

    #[test]
    fn gpu_freq_prefers_hwmon_input() {
        let mock = MockSysfs::new();
        seed_amdgpu(&mock);
        mock.create_file("sys/class/hwmon/hwmon5/freq1_input", "1100000000"); // 1100 MHz

        let backend = AsusTelemetryBackend::new(mock.clone());
        assert_eq!(backend.get_gpu_freq_mhz().unwrap(), Some(1100));
    }

    #[test]
    fn gpu_freq_falls_back_to_active_pp_dpm_sclk_line() {
        let mock = MockSysfs::new();
        seed_amdgpu(&mock);
        // No freq1_input on this driver.
        mock.create_file(
            "sys/class/hwmon/hwmon5/device/pp_dpm_sclk",
            "0: 200Mhz\n1: 700Mhz\n2: 1100Mhz *\n",
        );

        let backend = AsusTelemetryBackend::new(mock.clone());
        assert_eq!(backend.get_gpu_freq_mhz().unwrap(), Some(1100));
    }

    #[test]
    fn gpu_busy_and_vram() {
        let mock = MockSysfs::new();
        seed_amdgpu(&mock);
        mock.create_file("sys/class/hwmon/hwmon5/device/gpu_busy_percent", "73");
        mock.create_file(
            "sys/class/hwmon/hwmon5/device/mem_info_vram_used",
            "1073741824",
        ); // 1024 MB
        mock.create_file(
            "sys/class/hwmon/hwmon5/device/mem_info_vram_total",
            "17179869184",
        ); // 16384 MB

        let backend = AsusTelemetryBackend::new(mock.clone());
        assert_eq!(backend.get_gpu_busy_pct().unwrap(), Some(73));
        assert_eq!(backend.get_vram_used_mb().unwrap(), Some(1024));
        assert_eq!(backend.get_vram_total_mb().unwrap(), Some(16384));
    }

    #[test]
    fn gpu_fields_absent_without_amdgpu_node() {
        let mock = MockSysfs::new();
        let backend = AsusTelemetryBackend::new(mock.clone());
        assert_eq!(backend.get_gpu_freq_mhz().unwrap(), None);
        assert_eq!(backend.get_gpu_busy_pct().unwrap(), None);
        assert_eq!(backend.get_vram_used_mb().unwrap(), None);
        assert_eq!(backend.get_vram_total_mb().unwrap(), None);
    }

    #[test]
    fn gpu_throttle_status_defaults_to_none() {
        let mock = MockSysfs::new();
        seed_amdgpu(&mock);
        let backend = AsusTelemetryBackend::new(mock.clone());
        assert_eq!(backend.get_gpu_throttle_status().unwrap(), None);
    }
}
