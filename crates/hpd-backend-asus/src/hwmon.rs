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
}
