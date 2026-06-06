// SPDX-License-Identifier: GPL-3.0-or-later

use hpd_capabilities::charge::{ChargeControl, MAX_CHARGE_THRESHOLD, MIN_CHARGE_THRESHOLD};
use hpd_error::{BackendError, HpdError};
use hpd_sysfs::SysfsIo;

const BATTERY_PATH: &str = "/sys/class/power_supply/BAT0";
// Well-known AC/mains sysfs nodes across the ASUS handheld family. The
// node name is firmware/kernel dependent: the ROG Xbox Ally X (board
// RC73XA) exposes `AC0`, while earlier Ally/Ally X images use the bare
// `AC` or an `ADP*` node. We probe in order and take the first readable
// node — missing the device's actual name makes `is_ac_connected` fall
// through to the fail-safe `false`, so the daemon would report DC while
// physically on AC (the cause of the "AC status never updates" bug on
// the Xbox Ally X, whose node is `AC0`).
const AC_PATHS: [&str; 6] = [
    "/sys/class/power_supply/AC0/online",
    "/sys/class/power_supply/AC1/online",
    "/sys/class/power_supply/AC/online",
    "/sys/class/power_supply/ACAD/online",
    "/sys/class/power_supply/ADP0/online",
    "/sys/class/power_supply/ADP1/online",
];

/// [`ChargeControl`] implementation for ASUS handhelds.
///
/// Reads `BAT0/charge_control_end_threshold` for the limit and probes
/// the well-known AC sysfs nodes (`AC`, `ACAD`, `ADP0`, `ADP1`) to
/// decide whether the charger is currently plugged in.
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
        for path in AC_PATHS.iter() {
            if let Ok(val_str) = self.sysfs.read_string(path) {
                return Ok(val_str.trim() == "1");
            }
        }

        // Fail-Safe
        Ok(false)
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
        // not the bare `AC`/`ADP*` names. Missing it made is_ac_connected
        // always report false (DC) while physically plugged.
        let mock = MockSysfs::new();
        mock.create_file("sys/class/power_supply/AC0/online", "1");
        let backend = AsusChargeBackend::new(mock.clone());
        assert!(
            backend.is_ac_connected().unwrap(),
            "AC0/online == 1 must read as plugged"
        );
    }

    #[test]
    fn ac_unplugged_reads_false() {
        let mock = MockSysfs::new();
        mock.create_file("sys/class/power_supply/AC0/online", "0");
        let backend = AsusChargeBackend::new(mock.clone());
        assert!(!backend.is_ac_connected().unwrap());
    }

    #[test]
    fn missing_ac_node_falls_back_to_dc() {
        let mock = MockSysfs::new();
        // No AC node seeded at all.
        let backend = AsusChargeBackend::new(mock.clone());
        assert!(
            !backend.is_ac_connected().unwrap(),
            "absent mains node must fail safe to DC"
        );
    }
}
