// SPDX-License-Identifier: GPL-3.0-or-later

//! hwmon device lookup by `name`.
//!
//! The kernel assigns `/sys/class/hwmon/hwmonN` indices in registration
//! order, which is **not stable** across boots or driver-load order. On
//! a ROG Xbox Ally X, for example, the RPM-reading node (`asus`) and the
//! fan-curve node (`asus_custom_fan_curve`) land on different indices,
//! and an unrelated `acpi_fan` device also exposes a `fan1_input`. So we
//! never address a hwmon by index — we scan the class directory and
//! match on the `name` attribute, which *is* stable for a given driver.

use std::collections::HashMap;
use std::sync::Mutex;

use hpd_sysfs::SysfsIo;

/// Root of the hwmon class. Each child `hwmonN` is a symlink into the
/// owning device, but `/sys/class/hwmon/hwmonN/<attr>` resolves through
/// it, so addressing attributes from here is stable.
const HWMON_CLASS_DIR: &str = "/sys/class/hwmon";

/// Upper bound (exclusive) on hwmon indices we probe. Generous — a
/// handheld typically registers a dozen or so hwmon devices (battery,
/// AC, NVMe, amdgpu, k10temp, asus, asus_custom_fan_curve, USB-C PD,
/// Wi-Fi…), and the scan stops at the first name match anyway.
const MAX_HWMON_INDEX: u8 = 24;

/// Return the `/sys/class/hwmon/hwmonN` base path of the first hwmon
/// device whose `name` attribute equals `name`, or `None` if no such
/// device is present.
pub(crate) fn find_hwmon_by_name<S: SysfsIo>(sysfs: &S, name: &str) -> Option<String> {
    for i in 0..MAX_HWMON_INDEX {
        let base = format!("{HWMON_CLASS_DIR}/hwmon{i}");
        let name_path = format!("{base}/name");
        if let Ok(found) = sysfs.read_string(&name_path) {
            if found.trim() == name {
                return Some(base);
            }
        }
    }
    None
}

/// Per-instance cache of hwmon `name` → base path, so a backend polled at
/// ~1 Hz (temperatures, fan RPM, telemetry) does not re-scan up to
/// [`MAX_HWMON_INDEX`] sysfs nodes on every read. hwmon indices are only
/// reassigned between boots or a driver reload, so [`Self::resolve`]
/// trusts a cache hit after one cheap confirmation read of `{base}/name`
/// (much less work than a full rescan, and correctly tells "index
/// reassigned" apart from "this particular attribute is legitimately
/// absent on this device" — a read failure on the *attribute* itself,
/// handled by callers, is not grounds to invalidate the node's path).
#[derive(Default)]
pub(crate) struct HwmonCache {
    resolved: Mutex<HashMap<&'static str, String>>,
}

impl HwmonCache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Lock `resolved`, recovering the guard even if a prior holder
    /// panicked while it was held — the cache is a pure perf
    /// optimisation (a `None` or empty result just falls back to a full
    /// rescan), so there is nothing here worth propagating a panic over.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<&'static str, String>> {
        self.resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Resolve `name`'s hwmon base path, preferring a cached hit
    /// (confirmed still valid) over a full rescan.
    fn resolve<S: SysfsIo>(&self, sysfs: &S, name: &'static str) -> Option<String> {
        {
            let mut cache = self.lock();
            if let Some(base) = cache.get(name) {
                let still_valid = sysfs
                    .read_string(format!("{base}/name"))
                    .map(|found| found.trim() == name)
                    .unwrap_or(false);
                if still_valid {
                    return Some(base.clone());
                }
                cache.remove(name);
            }
        }
        let found = find_hwmon_by_name(sysfs, name);
        if let Some(base) = &found {
            self.lock().insert(name, base.clone());
        }
        found
    }

    /// Read `{base}/{attr}` under the hwmon node `name`, resolving `name`
    /// through the cache. `Ok(None)` when the node itself is absent, or
    /// when it exists but does not expose `attr` (e.g. a single-fan
    /// model has no `fan2_input`) — the same "hardware doesn't have
    /// this" contract every telemetry getter uses, so callers can treat
    /// `Ok(None)` uniformly regardless of *which* piece was missing.
    pub(crate) fn read_attr<S: SysfsIo>(
        &self,
        sysfs: &S,
        name: &'static str,
        attr: &str,
    ) -> Result<Option<String>, hpd_error::HpdError> {
        let Some(base) = self.resolve(sysfs, name) else {
            return Ok(None);
        };
        let path = format!("{base}/{attr}");
        if !sysfs.exists(&path) {
            return Ok(None);
        }
        Ok(Some(sysfs.read_string(&path)?))
    }

    /// Resolve `name`'s hwmon base path through the cache, for callers
    /// that need the path itself (e.g. to read a device-relative file
    /// under `{base}/device/...`) rather than a single attribute read.
    pub(crate) fn resolve_base<S: SysfsIo>(&self, sysfs: &S, name: &'static str) -> Option<String> {
        self.resolve(sysfs, name)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_sysfs::MockSysfs;

    #[test]
    fn finds_node_by_name_not_index() {
        let mock = MockSysfs::new();
        // Mirror the Xbox Ally X layout: acpi_fan at a low index, the
        // real asus node higher up.
        mock.create_file("sys/class/hwmon/hwmon1/name", "acpi_fan");
        mock.create_file("sys/class/hwmon/hwmon7/name", "asus");
        mock.create_file("sys/class/hwmon/hwmon8/name", "asus_custom_fan_curve");

        assert_eq!(
            find_hwmon_by_name(&mock, "asus"),
            Some("/sys/class/hwmon/hwmon7".to_string())
        );
        assert_eq!(
            find_hwmon_by_name(&mock, "asus_custom_fan_curve"),
            Some("/sys/class/hwmon/hwmon8".to_string())
        );
        assert_eq!(find_hwmon_by_name(&mock, "nonexistent"), None);
    }

    #[test]
    fn cached_path_reflects_live_value_changes() {
        let mock = MockSysfs::new();
        mock.create_file("sys/class/hwmon/hwmon5/name", "amdgpu");
        mock.create_file("sys/class/hwmon/hwmon5/temp1_input", "72000");

        let cache = HwmonCache::new();
        assert_eq!(
            cache.read_attr(&mock, "amdgpu", "temp1_input").unwrap(),
            Some("72000".to_string())
        );

        // A live sensor's value changes on every poll; only the resolved
        // *path* may be cached, never the attribute's value.
        mock.create_file("sys/class/hwmon/hwmon5/temp1_input", "65000");
        assert_eq!(
            cache.read_attr(&mock, "amdgpu", "temp1_input").unwrap(),
            Some("65000".to_string())
        );
    }

    #[test]
    fn stale_cached_path_invalidates_and_rescans() {
        // Simulate a driver reload reassigning the `amdgpu` hwmon from
        // index 5 to index 9.
        let mock = MockSysfs::new();
        mock.create_file("sys/class/hwmon/hwmon5/name", "amdgpu");
        mock.create_file("sys/class/hwmon/hwmon5/temp1_input", "72000");

        let cache = HwmonCache::new();
        assert_eq!(
            cache.read_attr(&mock, "amdgpu", "temp1_input").unwrap(),
            Some("72000".to_string())
        );

        // The node moves: the old index vanishes entirely, a new one
        // appears with the same name but a fresh reading.
        mock.remove_path("sys/class/hwmon/hwmon5");
        mock.create_file("sys/class/hwmon/hwmon9/name", "amdgpu");
        mock.create_file("sys/class/hwmon/hwmon9/temp1_input", "65000");

        assert_eq!(
            cache.read_attr(&mock, "amdgpu", "temp1_input").unwrap(),
            Some("65000".to_string()),
            "a read failure against a cached path must invalidate and rescan"
        );
    }

    #[test]
    fn absent_node_reads_as_none_without_poisoning_the_cache() {
        let mock = MockSysfs::new();
        let cache = HwmonCache::new();
        assert_eq!(
            cache.read_attr(&mock, "amdgpu", "temp1_input").unwrap(),
            None
        );

        // Once the node appears, the same cache must still find it.
        mock.create_file("sys/class/hwmon/hwmon2/name", "amdgpu");
        mock.create_file("sys/class/hwmon/hwmon2/temp1_input", "50000");
        assert_eq!(
            cache.read_attr(&mock, "amdgpu", "temp1_input").unwrap(),
            Some("50000".to_string())
        );
    }
}
