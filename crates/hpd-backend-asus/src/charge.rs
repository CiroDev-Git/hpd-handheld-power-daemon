// SPDX-License-Identifier: GPL-3.0-or-later

use hpd_capabilities::charge::{ChargeControl, MAX_CHARGE_THRESHOLD, MIN_CHARGE_THRESHOLD};
use hpd_error::{BackendError, HpdError};
use hpd_sysfs::SysfsIo;

use crate::power_supply::find_nodes_by_type;

const BATTERY_PATH: &str = "/sys/class/power_supply/BAT0";

/// [`ChargeControl`] implementation for ASUS handhelds.
///
/// Reads `BAT0/charge_control_end_threshold` for the limit and scans
/// `power_supply` for a `type == "Mains"` node to decide whether the
/// charger is currently plugged in.
pub struct AsusChargeBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> AsusChargeBackend<S> {
    /// Wrap a `SysfsIo` handle. Free in production (`RealSysfs` is
    /// zero-sized); cheap in tests (`MockSysfs` shares its `TempDir`
    /// via `Arc`).
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }
}

impl<S: SysfsIo> ChargeControl for AsusChargeBackend<S> {
    fn is_ac_connected(&self) -> Result<bool, HpdError> {
        // AC/mains node *names* are firmware/kernel dependent (the ROG
        // Xbox Ally X exposes `AC0`; earlier Ally/Ally X images use `AC`
        // or an `ADP*` node), so scan by `type` instead of a fixed name
        // list — mirroring `hpd-netlink`'s `read_mains_online_at`, which
        // independently arrived at the same approach for the same reason
        // (a fixed name list previously caused the "AC status never
        // updates" bug on the Xbox Ally X, whose node is `AC0` and
        // wasn't in the old list). Every `Mains`-typed node is checked
        // (not just the first): a board with more than one (e.g. `AC0` +
        // `AC1`) counts as plugged in if any reports `online == 1`.
        Ok(find_nodes_by_type(&self.sysfs, "Mains").iter().any(|base| {
            self.sysfs
                .read_string(format!("{base}/online"))
                .unwrap_or_default()
                .trim()
                == "1"
        }))
    }

    fn get_end_threshold(&self) -> Result<u8, HpdError> {
        let path = format!("{}/charge_control_end_threshold", BATTERY_PATH);
        let val_str = self.sysfs.read_string(&path)?;
        let threshold: u8 = val_str.parse().map_err(|_| BackendError::ParseFailed {
            field: "charge_end_threshold",
            raw: val_str.clone(),
            reason: "expected u8 (0-100)".into(),
        })?;
        Ok(threshold)
    }

    fn set_end_threshold(&self, threshold: u8) -> Result<(), HpdError> {
        if !(MIN_CHARGE_THRESHOLD..=MAX_CHARGE_THRESHOLD).contains(&threshold) {
            return Err(HpdError::InvariantViolation(format!(
                "charge threshold must be between {} and {}, got {}",
                MIN_CHARGE_THRESHOLD, MAX_CHARGE_THRESHOLD, threshold
            )));
        }

        let path = format!("{}/charge_control_end_threshold", BATTERY_PATH);
        self.sysfs.write_string(&path, &threshold.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_sysfs::MockSysfs;

    #[test]
    fn detects_ac0_node_on_xbox_ally_x() {
        // Regression: the Xbox Ally X exposes its mains node as `AC0`,
        // not the bare `AC`/`ADP*` names. Scanning by `type == "Mains"`
        // (instead of a fixed name list) finds it regardless of name.
        let mock = MockSysfs::new();
        mock.create_file("sys/class/power_supply/AC0/type", "Mains");
        mock.create_file("sys/class/power_supply/AC0/online", "1");
        let backend = AsusChargeBackend::new(mock.clone());
        assert!(
            backend.is_ac_connected().unwrap(),
            "a Mains node with online == 1 must read as plugged"
        );
    }

    #[test]
    fn ac_unplugged_reads_false() {
        let mock = MockSysfs::new();
        mock.create_file("sys/class/power_supply/AC0/type", "Mains");
        mock.create_file("sys/class/power_supply/AC0/online", "0");
        let backend = AsusChargeBackend::new(mock.clone());
        assert!(!backend.is_ac_connected().unwrap());
    }

    #[test]
    fn missing_ac_node_falls_back_to_dc() {
        let mock = MockSysfs::new();
        // No power_supply nodes seeded at all.
        let backend = AsusChargeBackend::new(mock.clone());
        assert!(
            !backend.is_ac_connected().unwrap(),
            "absent mains node must fail safe to DC"
        );
    }

    #[test]
    fn usb_c_pd_node_is_not_mistaken_for_mains() {
        // Regression for Audit §4.1 (2026-07): before the scan-by-type
        // unification, a USB-C PD port reporting online=1 under a name
        // that happened to look AC-ish would never have been read (the
        // old fixed-path list only probed AC*/ADP* names) — but the risk
        // ran the other way too: a naive "any online==1 node" scan (unlike
        // one gated on type=="Mains") would wrongly count a charging-only
        // USB device as AC. Pin the actual contract: a `type == "USB"`
        // node with online=1 must NOT count as AC.
        let mock = MockSysfs::new();
        mock.create_file(
            "sys/class/power_supply/ucsi-source-psy-USBC000:002/type",
            "USB",
        );
        mock.create_file(
            "sys/class/power_supply/ucsi-source-psy-USBC000:002/online",
            "1",
        );
        let backend = AsusChargeBackend::new(mock.clone());
        assert!(
            !backend.is_ac_connected().unwrap(),
            "a USB (non-Mains) node reporting online=1 must not count as AC"
        );
    }

    #[test]
    fn one_offline_mains_node_among_several_does_not_shadow_an_online_one() {
        // Multiple Mains-typed nodes (e.g. AC0 + AC1) must be scanned in
        // full, not short-circuited on the first one found: whichever
        // node is actually online should still be detected as plugged.
        let mock = MockSysfs::new();
        mock.create_file("sys/class/power_supply/AC0/type", "Mains");
        mock.create_file("sys/class/power_supply/AC0/online", "0");
        mock.create_file("sys/class/power_supply/AC1/type", "Mains");
        mock.create_file("sys/class/power_supply/AC1/online", "1");
        let backend = AsusChargeBackend::new(mock.clone());
        assert!(backend.is_ac_connected().unwrap());
    }
}
