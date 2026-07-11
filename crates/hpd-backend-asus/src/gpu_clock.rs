// SPDX-License-Identifier: GPL-3.0-or-later

//! [`GpuClockRangeControl`] implementation for ASUS AMD handhelds.
//!
//! Drives amdgpu's OverDrive interface: `power_dpm_force_performance_level`
//! (must be `manual` before any OD write is honoured — confirmed via a
//! real ROG Xbox Ally X, where OverDrive is already enabled by the
//! shipped `ppfeaturemask`, no kernel boot parameter needed) and
//! `pp_od_clk_voltage` (the actual SCLK min/max range; its `OD_RANGE`
//! block reports the device's supported bounds live). Both sysfs files
//! sit next to the `gpu_busy_percent`/`pp_dpm_sclk` attributes
//! `telemetry.rs` already reads, resolved through the same `HwmonCache`.
//!
//! ## Write sequence + failure handling
//!
//! [`AsusGpuClockBackend::set_range`](crate::gpu_clock::AsusGpuClockBackend::set_range)
//! validates against the live
//! `OD_RANGE` first, switches to `manual` only if not already there,
//! writes `"s 0 {min}"` / `"s 1 {max}"` / `"c"` (commit), then reads back
//! `OD_SCLK` to confirm the driver accepted it. **On any failure past
//! validation, this method cleans up internally** — best-effort
//! [`reset_to_auto`](crate::gpu_clock::AsusGpuClockBackend::reset_to_auto)
//! before propagating the original
//! error — so it never returns having left the device in `manual` mode
//! with no valid committed range (the one genuinely unsafe intermediate
//! state). This keeps `Executor::rollback` completely generic: every
//! `build_sync_transition` closure it calls today is read-only, and this
//! backend's own internal cleanup is what preserves that invariant rather
//! than teaching the executor to perform a write.

use hpd_capabilities::gpu_clock::{GpuClockConstraints, GpuClockRange, GpuClockRangeControl};
use hpd_error::{BackendError, HpdError};
use hpd_sysfs::SysfsIo;

use crate::hwmon::HwmonCache;

/// hwmon `name` of the AMD GPU driver — same node `telemetry.rs`/`thermal.rs`
/// each independently resolve (duplicated per-file by existing
/// convention, not shared, to keep each capability module self-contained).
const GPU_HWMON_NAME: &str = "amdgpu";

const FORCE_LEVEL_ATTR: &str = "power_dpm_force_performance_level";
const OD_CLK_VOLTAGE_ATTR: &str = "pp_od_clk_voltage";
const MODE_MANUAL: &str = "manual";
const MODE_AUTO: &str = "auto";

/// [`GpuClockRangeControl`] implementation for ASUS AMD handhelds.
pub struct AsusGpuClockBackend<S: SysfsIo> {
    sysfs: S,
    hwmon_cache: HwmonCache,
}

impl<S: SysfsIo> AsusGpuClockBackend<S> {
    /// Wrap a `SysfsIo` handle (see [`AsusBackend::new`](crate::AsusBackend::new)).
    pub fn new(sysfs: S) -> Self {
        Self {
            sysfs,
            hwmon_cache: HwmonCache::new(),
        }
    }

    /// Resolve the `amdgpu` hwmon's sibling DRM device directory, or
    /// [`HpdError::FeatureUnsupported`] when the node is absent.
    fn gpu_base(&self) -> Result<String, HpdError> {
        self.hwmon_cache
            .resolve_base(&self.sysfs, GPU_HWMON_NAME)
            .ok_or(HpdError::FeatureUnsupported)
    }

    fn force_level_path(base: &str) -> String {
        format!("{base}/device/{FORCE_LEVEL_ATTR}")
    }

    fn od_path(base: &str) -> String {
        format!("{base}/device/{OD_CLK_VOLTAGE_ATTR}")
    }

    fn read_force_level(&self, base: &str) -> Result<String, HpdError> {
        Ok(self
            .sysfs
            .read_string(Self::force_level_path(base))?
            .trim()
            .to_string())
    }

    fn write_force_level(&self, base: &str, level: &str) -> Result<(), HpdError> {
        self.sysfs
            .write_string(Self::force_level_path(base), level)?;
        Ok(())
    }

    fn read_od(&self, base: &str) -> Result<String, HpdError> {
        Ok(self.sysfs.read_string(Self::od_path(base))?)
    }

    /// Read `OD_RANGE`'s `SCLK:` line — the device's supported bounds.
    fn read_constraints(&self, base: &str) -> Result<GpuClockConstraints, HpdError> {
        let raw = self.read_od(base)?;
        for line in raw.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("SCLK:") {
                let nums: Vec<u32> = rest.split_whitespace().filter_map(mhz_value).collect();
                if nums.len() == 2 {
                    return Ok(GpuClockConstraints {
                        range_min_mhz: nums[0],
                        range_max_mhz: nums[1],
                    });
                }
            }
        }
        Err(parse_error(
            "pp_od_clk_voltage_od_range",
            &raw,
            "expected a `SCLK:` line inside `OD_RANGE`",
        ))
    }

    /// Read `OD_SCLK`'s `0:`/`1:` lines — the currently committed range.
    /// `None` fields in the block (e.g. the driver hasn't reported one
    /// yet) surface as a parse error, matching the read-back-and-fail-
    /// closed discipline `set_range` needs.
    fn read_committed_sclk(&self, base: &str) -> Result<GpuClockRange, HpdError> {
        let raw = self.read_od(base)?;
        let mut min_mhz = None;
        let mut max_mhz = None;
        for line in raw.lines() {
            let line = line.trim();
            if line.starts_with("OD_RANGE") {
                break; // OD_SCLK is only ever reported before OD_RANGE
            }
            if let Some(rest) = line.strip_prefix("0:") {
                min_mhz = rest.split_whitespace().next().and_then(mhz_value);
            } else if let Some(rest) = line.strip_prefix("1:") {
                max_mhz = rest.split_whitespace().next().and_then(mhz_value);
            }
        }
        match (min_mhz, max_mhz) {
            (Some(min_mhz), Some(max_mhz)) => Ok(GpuClockRange { min_mhz, max_mhz }),
            _ => Err(parse_error(
                "pp_od_clk_voltage_od_sclk",
                &raw,
                "expected `0:`/`1:` lines inside `OD_SCLK`",
            )),
        }
    }
}

impl<S: SysfsIo> GpuClockRangeControl for AsusGpuClockBackend<S> {
    fn set_range(&self, range: &GpuClockRange) -> Result<(), HpdError> {
        let base = self.gpu_base()?;
        let constraints = self.read_constraints(&base)?;
        range.validate_against(&constraints)?;

        let result = (|| -> Result<(), HpdError> {
            if self.read_force_level(&base)? != MODE_MANUAL {
                self.write_force_level(&base, MODE_MANUAL)?;
            }
            self.sysfs
                .write_string(Self::od_path(&base), &format!("s 0 {}", range.min_mhz))?;
            self.sysfs
                .write_string(Self::od_path(&base), &format!("s 1 {}", range.max_mhz))?;
            self.sysfs.write_string(Self::od_path(&base), "c")?;

            let committed = self.read_committed_sclk(&base)?;
            if committed != *range {
                return Err(HpdError::Backend(BackendError::Other(format!(
                    "gpu clock read-back mismatch: wrote {range:?}, driver reports {committed:?}"
                ))));
            }
            Ok(())
        })();

        if result.is_err() {
            // Never leave the device stuck in `manual` with no valid
            // committed range — best-effort cleanup. This L1 crate has no
            // logging facility of its own (unlike hpd-core/hpd-daemon,
            // which log via `tracing`); a cleanup failure here is silently
            // swallowed rather than masking the original error, which is
            // what the caller actually needs to see.
            let _ = self.reset_to_auto();
        }
        result
    }

    fn reset_to_auto(&self) -> Result<(), HpdError> {
        let base = self.gpu_base()?;
        // "r" resets OD_SCLK to the firmware default while still in
        // manual mode; only then hand DPM back to auto. Exact ordering
        // requirements aren't documented upstream — this is the on-device
        // QA gate's other open question (see CLAUDE.md).
        self.sysfs.write_string(Self::od_path(&base), "r")?;
        self.write_force_level(&base, MODE_AUTO)?;
        Ok(())
    }

    fn active_range(&self) -> Result<Option<GpuClockRange>, HpdError> {
        let base = self.gpu_base()?;
        if self.read_force_level(&base)? == MODE_AUTO {
            return Ok(None);
        }
        Ok(Some(self.read_committed_sclk(&base)?))
    }

    fn constraints(&self) -> Result<GpuClockConstraints, HpdError> {
        let base = self.gpu_base()?;
        self.read_constraints(&base)
    }
}

/// Extract the leading digits of a whitespace-delimited token (e.g.
/// `"600Mhz"` -> `600`), matching `telemetry.rs`'s
/// `parse_active_dpm_mhz` convention for the same kind of `NMhz` token.
fn mhz_value(token: &str) -> Option<u32> {
    let digits: String = token.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

fn parse_error(field: &'static str, raw: &str, reason: &str) -> HpdError {
    HpdError::Backend(BackendError::ParseFailed {
        field,
        raw: raw.to_string(),
        reason: reason.to_string(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_sysfs::MockSysfs;

    /// Seed a mock `amdgpu` hwmon node with `power_dpm_force_performance_level`
    /// (`auto`) and `pp_od_clk_voltage` matching the real capture from a
    /// ROG Xbox Ally X (`OD_RANGE: SCLK 600Mhz-2900Mhz`).
    fn seed_gpu_node(mock: &MockSysfs) {
        mock.create_file("sys/class/hwmon/hwmon5/name", "amdgpu");
        mock.create_file(
            "sys/class/hwmon/hwmon5/device/power_dpm_force_performance_level",
            "auto",
        );
        mock.create_file(
            "sys/class/hwmon/hwmon5/device/pp_od_clk_voltage",
            "OD_SCLK:\n0:        600Mhz\n1:       2900Mhz\nOD_RANGE:\nSCLK:     600Mhz       2900Mhz\n",
        );
    }

    fn od_path() -> &'static str {
        "/sys/class/hwmon/hwmon5/device/pp_od_clk_voltage"
    }

    fn force_level_path() -> &'static str {
        "/sys/class/hwmon/hwmon5/device/power_dpm_force_performance_level"
    }

    /// A `pp_od_clk_voltage` write in this mock is "s"/"r" free-form —
    /// `MockSysfs` just stores whatever is written. To simulate the
    /// kernel actually committing a range, tests write the post-commit
    /// `OD_SCLK` content directly rather than modelling the driver's own
    /// `s`/`c` state machine (out of scope for a sysfs mock).
    fn simulate_committed(mock: &MockSysfs, min_mhz: u32, max_mhz: u32) {
        mock.write_string(
            od_path(),
            &format!(
                "OD_SCLK:\n0:        {min_mhz}Mhz\n1:       {max_mhz}Mhz\nOD_RANGE:\nSCLK:     600Mhz       2900Mhz\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn constraints_reads_od_range() {
        let mock = MockSysfs::new();
        seed_gpu_node(&mock);
        let backend = AsusGpuClockBackend::new(mock.clone());
        let c = backend.constraints().unwrap();
        assert_eq!(c.range_min_mhz, 600);
        assert_eq!(c.range_max_mhz, 2900);
    }

    #[test]
    fn active_range_is_none_in_auto_mode() {
        let mock = MockSysfs::new();
        seed_gpu_node(&mock);
        let backend = AsusGpuClockBackend::new(mock.clone());
        assert_eq!(backend.active_range().unwrap(), None);
    }

    #[test]
    fn missing_gpu_node_is_feature_unsupported() {
        let mock = MockSysfs::new();
        let backend = AsusGpuClockBackend::new(mock.clone());
        assert!(matches!(
            backend.constraints(),
            Err(HpdError::FeatureUnsupported)
        ));
    }

    #[test]
    fn set_range_rejects_out_of_bounds_before_any_write() {
        let mock = MockSysfs::new();
        seed_gpu_node(&mock);
        let backend = AsusGpuClockBackend::new(mock.clone());

        let bad = GpuClockRange {
            min_mhz: 600,
            max_mhz: 3200, // above the device's 2900 MHz ceiling
        };
        assert!(backend.set_range(&bad).is_err());
        // Force-level must be untouched — validation happens before any write.
        assert_eq!(mock.read_string(force_level_path()).unwrap(), "auto");
    }

    // `MockSysfs::write_string` replaces a path's entire content with
    // exactly what's written (no real driver state machine behind
    // amdgpu's "s"/"c" protocol) — so against a bare mock, `set_range`'s
    // own post-commit read-back never matches what was requested, and
    // the write sequence's fail-closed + cleanup path is exactly what
    // fires. That's a real, useful assertion in its own right (the
    // read-back-or-clean-up discipline), even though it can't exercise
    // the "driver genuinely accepted the range" success path — that needs
    // the real driver, covered by the on-device QA gate instead.
    #[test]
    fn set_range_fails_closed_and_resets_to_auto_when_the_readback_does_not_match() {
        let mock = MockSysfs::new();
        seed_gpu_node(&mock);
        let backend = AsusGpuClockBackend::new(mock.clone());

        let range = GpuClockRange {
            min_mhz: 800,
            max_mhz: 1800,
        };
        assert!(backend.set_range(&range).is_err());
        // Cleanup must have reset force-level back to auto rather than
        // leaving the device stuck in manual with no valid committed range.
        assert_eq!(mock.read_string(force_level_path()).unwrap(), "auto");
    }

    #[test]
    fn reset_to_auto_writes_r_then_auto() {
        let mock = MockSysfs::new();
        seed_gpu_node(&mock);
        mock.write_string(force_level_path(), "manual").unwrap();
        let backend = AsusGpuClockBackend::new(mock.clone());

        backend.reset_to_auto().unwrap();
        assert_eq!(mock.read_string(force_level_path()).unwrap(), "auto");
    }

    #[test]
    fn active_range_reports_custom_when_manual_and_committed() {
        let mock = MockSysfs::new();
        seed_gpu_node(&mock);
        mock.write_string(force_level_path(), "manual").unwrap();
        simulate_committed(&mock, 700, 2200);
        let backend = AsusGpuClockBackend::new(mock.clone());

        assert_eq!(
            backend.active_range().unwrap(),
            Some(GpuClockRange {
                min_mhz: 700,
                max_mhz: 2200
            })
        );
    }
}
