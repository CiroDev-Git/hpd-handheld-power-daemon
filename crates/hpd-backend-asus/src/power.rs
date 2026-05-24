// SPDX-License-Identifier: GPL-3.0-or-later

use hpd_capabilities::power::{PowerEnvelope, PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_capabilities::units::PowerMilliwatts;
use hpd_error::{BackendError, HpdError};
use hpd_sysfs::SysfsIo;

const BASE_PATH: &str = "/sys/class/firmware-attributes/asus-armoury/attributes";

// Canonical attribute names exposed by the upstream `asus-armoury` driver.
// Verified on ROG Xbox Ally X (board RC73XA) against Linux's
// drivers/platform/x86/asus-armoury/.
const ATTR_SPL: &str = "ppt_pl1_spl";
const ATTR_SPPT: &str = "ppt_pl2_sppt";
const ATTR_FPPT: &str = "ppt_pl3_fppt";

// Fallback boost-rail maxima for ASUS handhelds when `max_value` is not
// exposed by the driver. Documented values for the ROG Ally / Ally X /
// Xbox Ally X family.
const ASUS_DEFAULT_SPPT_MAX_MW: u32 = 43_000;
const ASUS_DEFAULT_FPPT_MAX_MW: u32 = 53_000;

pub struct AsusPowerBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> AsusPowerBackend<S> {
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }

    fn read_watts(&self, attr: &str, suffix: &str) -> Result<PowerMilliwatts, HpdError> {
        let path = format!("{}/{}/{}", BASE_PATH, attr, suffix);
        let val_str = self.sysfs.read_string(&path)?;
        let watts: u32 = val_str.parse().map_err(|_| BackendError::ParseFailed {
            field: "watts",
            raw: val_str.clone(),
            reason: format!("expected integer at {}", path),
        })?;
        // Convert from W (kernel) to mW (domain).
        PowerMilliwatts::from_watts(watts)
    }

    fn write_watts(&self, attr: &str, target_mw: PowerMilliwatts) -> Result<(), HpdError> {
        let path = format!("{}/{}/current_value", BASE_PATH, attr);
        // Convert from mW (domain) to W (kernel).
        let watts = target_mw.as_watts();
        self.sysfs.write_string(&path, &watts.to_string())?;
        Ok(())
    }
}

impl<S: SysfsIo> PowerEnvelope for AsusPowerBackend<S> {
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError> {
        let spl_min = self.read_watts(ATTR_SPL, "min_value")?;
        let spl_max = self.read_watts(ATTR_SPL, "max_value")?;

        // Fallbacks for hardware that doesn't expose the max attribute.
        let sppt_max = self
            .read_watts(ATTR_SPPT, "max_value")
            .unwrap_or(PowerMilliwatts(ASUS_DEFAULT_SPPT_MAX_MW));
        let fppt_max = self
            .read_watts(ATTR_FPPT, "max_value")
            .unwrap_or(PowerMilliwatts(ASUS_DEFAULT_FPPT_MAX_MW));

        Ok(PowerEnvelopeLimits {
            spl_min,
            spl_max,
            sppt_max,
            fppt_max,
        })
    }

    fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError> {
        let spl = self.read_watts(ATTR_SPL, "current_value")?;
        let sppt = self.read_watts(ATTR_SPPT, "current_value")?;
        let fppt = self.read_watts(ATTR_FPPT, "current_value")?;

        Ok(PowerEnvelopeTarget {
            spl,
            sppt,
            fppt: Some(fppt),
        })
    }

    fn set_target(&self, target: &PowerEnvelopeTarget) -> Result<(), HpdError> {
        self.write_watts(ATTR_SPL, target.spl)?;
        self.write_watts(ATTR_SPPT, target.sppt)?;

        if let Some(fppt) = target.fppt {
            self.write_watts(ATTR_FPPT, fppt)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // Test code may use `.unwrap()` / `.expect()` / `panic!` freely;
    // the strict bar in `[workspace.lints.clippy]` applies to
    // production code only.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_capabilities::units::PowerMilliwatts;
    use hpd_sysfs::MockSysfs; // Simulator based on TempDir

    #[test]
    fn test_asus_power_translation_mw_to_watts() {
        // 1. Arrange: Prepare system with fake files
        let mock = MockSysfs::new();

        // MockSysfs strips the leading '/' when handling absolute paths.
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value",
            "15",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value",
            "15",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl3_fppt/current_value",
            "15",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/min_value",
            "7",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/max_value",
            "35",
        );
        // Canonical max attributes for the boost rails.
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/max_value",
            "43",
        );
        mock.create_file(
            "sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl3_fppt/max_value",
            "55",
        );

        let backend = AsusPowerBackend::new(mock.clone());

        // 2. Act & Assert (Read): Check that "15" on disk is read as 15000mW
        let target = backend.get_target().expect("Must be able to read target");
        assert_eq!(target.spl, PowerMilliwatts(15000));
        assert_eq!(target.sppt, PowerMilliwatts(15000));

        let limits = backend
            .get_limits()
            .expect("Must be able to read the limits");
        assert_eq!(limits.spl_min, PowerMilliwatts(7000));
        assert_eq!(limits.spl_max, PowerMilliwatts(35000));
        assert_eq!(limits.sppt_max, PowerMilliwatts(43000));
        // Regression for the ppt_fppt vs ppt_pl3_fppt bug (Audit §3.2 / Lote 4).
        // If get_limits reads the wrong attribute it falls back silently to
        // ASUS_DEFAULT_FPPT_MAX_MW (53000), so this `55_000` assertion is
        // what proves the canonical attribute is being read.
        assert_eq!(limits.fppt_max, PowerMilliwatts(55000));

        // 3. Act & Assert (Write): write 25000mW and check that disk stored "25".
        let new_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(20000),
            sppt: PowerMilliwatts(25000),
            fppt: Some(PowerMilliwatts(30000)),
        };

        backend
            .set_target(&new_target)
            .expect("Must be able to write the target");

        // Use the mock to spy what was written in file
        let spl_written = mock
            .read_string(
                "/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value",
            )
            .unwrap();
        let sppt_written = mock
            .read_string(
                "/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value",
            )
            .unwrap();

        assert_eq!(spl_written, "20", "20000mW must translate to string '20'");
        assert_eq!(sppt_written, "25", "25000mW must translate to string '25'");
    }
}
