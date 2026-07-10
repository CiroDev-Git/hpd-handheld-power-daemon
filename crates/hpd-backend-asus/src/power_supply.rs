// SPDX-License-Identifier: GPL-3.0-or-later

//! `power_supply` class node lookup by `type`.
//!
//! Mirrors `hwmon`'s "never address by a fixed name/index" lesson: the AC
//! node's name is firmware/kernel dependent (`AC0` on the Xbox Ally X,
//! `AC`/`ADP*` on earlier Ally images), and the same is true of the
//! battery node in principle even though every ASUS handheld today
//! happens to expose `BAT0`. Scanning `/sys/class/power_supply` for a
//! `type` match is the one algorithm that is correct for both, and for
//! any future device whose kernel names the node differently.

use hpd_sysfs::SysfsIo;

/// The kernel `power_supply` class root.
pub(crate) const POWER_SUPPLY_ROOT: &str = "/sys/class/power_supply";

/// Return the base paths of every `power_supply` node under
/// [`POWER_SUPPLY_ROOT`] whose `type` attribute equals `kind` (e.g.
/// `"Mains"`, `"Battery"`). Callers that must consider every matching
/// node (e.g. AC detection, where any of several `Mains` nodes reporting
/// `online == 1` counts as plugged in) use this directly; callers that
/// only need one node use [`find_node_by_type`].
pub(crate) fn find_nodes_by_type<S: SysfsIo>(sysfs: &S, kind: &str) -> Vec<String> {
    sysfs
        .read_dir_names(POWER_SUPPLY_ROOT)
        .into_iter()
        .map(|node| format!("{POWER_SUPPLY_ROOT}/{node}"))
        .filter(|base| {
            sysfs
                .read_string(format!("{base}/type"))
                .unwrap_or_default()
                .trim()
                == kind
        })
        .collect()
}

/// Return the first `power_supply` node under [`POWER_SUPPLY_ROOT`] whose
/// `type` attribute equals `kind`, or `None` if no such node exists.
/// Today's ASUS handhelds expose exactly one node of each type `hpd`
/// reads beyond AC (e.g. one `Battery`), so "first found" is enough.
pub(crate) fn find_node_by_type<S: SysfsIo>(sysfs: &S, kind: &str) -> Option<String> {
    find_nodes_by_type(sysfs, kind).into_iter().next()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_sysfs::MockSysfs;

    #[test]
    fn finds_node_by_type_regardless_of_name() {
        let mock = MockSysfs::new();
        mock.create_file("sys/class/power_supply/AC0/type", "Mains");
        mock.create_file("sys/class/power_supply/BAT1/type", "Battery");

        assert_eq!(
            find_node_by_type(&mock, "Mains"),
            Some("/sys/class/power_supply/AC0".to_string())
        );
        assert_eq!(
            find_node_by_type(&mock, "Battery"),
            Some("/sys/class/power_supply/BAT1".to_string())
        );
        assert_eq!(find_node_by_type(&mock, "USB"), None);
    }

    #[test]
    fn no_nodes_returns_none() {
        let mock = MockSysfs::new();
        assert_eq!(find_node_by_type(&mock, "Battery"), None);
    }
}
