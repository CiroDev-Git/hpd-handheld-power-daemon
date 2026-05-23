use hpd_capabilities::error::HpdError;
use hpd_capabilities::power::{PowerEnvelope, PowerEnvelopeLimits, PowerEnvelopeTarget};
use hpd_capabilities::units::PowerMilliwatts;
use hpd_sysfs::SysfsIo;

const BASE_PATH: &str = "/sys/class/firmware-attributes/asus-armoury/attributes";

pub struct AsusPowerBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> AsusPowerBackend<S> {
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }

    fn read_watts(&self, attr: &str, suffix: &str) -> Result<PowerMilliwatts, HpdError> {
        let path = format!("{}/{}/{}", BASE_PATH, attr, suffix);
        // Map error from L0 to generic error of L2
        let val_str = self.sysfs.read_string(&path).map_err(|e| HpdError::Backend { 
            reason: format!("Sysfs read failed at {}: {}", path, e) 
        })?;
        
        let watts: u32 = val_str.parse().map_err(|_| HpdError::Backend { 
            reason: format!("Failed to parse integer from {}", path) 
        })?;

        // Convert from W (kernel) to mW (domain).
        Ok(PowerMilliwatts(watts * 1000))
    }

    fn write_watts(&self, attr: &str, target_mw: PowerMilliwatts) -> Result<(), HpdError> {
        let path = format!("{}/{}/current_value", BASE_PATH, attr);
        // Convert from mW (domain) to W (kernel).
        let watts = target_mw.0 / 1000;
        // Map error from L0 to generic error of L2
        self.sysfs.write_string(&path, &watts.to_string()).map_err(|e| HpdError::Backend { 
            reason: format!("Sysfs write failed at {}: {}", path, e) 
        })?;
        Ok(())
    }
}

impl<S: SysfsIo> PowerEnvelope for AsusPowerBackend<S> {
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError> {
        let min = self.read_watts("ppt_pl1_spl", "min_value")?;
        let max = self.read_watts("ppt_pl1_spl", "max_value")?;

        let sppt_max = self.read_watts("ppt_pl2_sppt", "max_value")
            .unwrap_or(PowerMilliwatts(43000));
            
        let fppt_max = self.read_watts("ppt_fppt", "max_value")
            .unwrap_or(PowerMilliwatts(53000));

        Ok(PowerEnvelopeLimits {
            spl_min: min,
            spl_max: max,
            sppt_max: sppt_max,
            fppt_max:fppt_max,
        })
    }

    fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError> {
        let spl = self.read_watts("ppt_pl1_spl", "current_value")?;
        let sppt = self.read_watts("ppt_pl2_sppt", "current_value")?;
        let fppt = self.read_watts("ppt_pl3_fppt", "current_value")?; // FIXME(Lote 4): get_limits uses "ppt_fppt" — confirm and unify.

        Ok(PowerEnvelopeTarget {
            spl,
            sppt,
            fppt: Some(fppt),
        })
    }

    fn set_target(&self, target: &PowerEnvelopeTarget) -> Result<(), HpdError> {
        self.write_watts("ppt_pl1_spl", target.spl)?;
        self.write_watts("ppt_pl2_sppt", target.sppt)?;

        if let Some(fppt) = target.fppt {
            self.write_watts("ppt_pl3_fppt", fppt)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hpd_sysfs::MockSysfs; // Simulator based on TempDir
    use hpd_capabilities::units::PowerMilliwatts;

    #[test]
    fn test_asus_power_translation_mw_to_watts() {
        // 1. Arrange: Prepare system with fake files
        let mock = MockSysfs::new();
        
        // MockSysfs handle absolutes paths removing prefix '/'
        mock.create_file("sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value", "15");
        mock.create_file("sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value", "15");
        mock.create_file("sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl3_fppt/current_value", "15");
        mock.create_file("sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/min_value", "7");
        mock.create_file("sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/max_value", "35");

        let backend = AsusPowerBackend::new(mock.clone());

        // 2. Act & Assert (Read): Check that "15" on disk is read as 15000mW
        let target = backend.get_target().expect("Must be able to read target");
        assert_eq!(target.spl, PowerMilliwatts(15000));
        assert_eq!(target.sppt, PowerMilliwatts(15000));

        let limits = backend.get_limits().expect("Must be able to read the limits");
        assert_eq!(limits.spl_min, PowerMilliwatts(7000));
        assert_eq!(limits.spl_max, PowerMilliwatts(35000));

        // 3. Act & Assert (Write): Write 25000mW y check that disk stored "25"
        let new_target = PowerEnvelopeTarget {
            spl: PowerMilliwatts(20000),
            sppt: PowerMilliwatts(25000),
            fppt: Some(PowerMilliwatts(30000)),
        };

        backend.set_target(&new_target).expect("Must be able to write the target");

        // Use the mock to spy what was written in file
        let spl_written = mock.read_string("/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl1_spl/current_value")
            .unwrap();
        let sppt_written = mock.read_string("/sys/class/firmware-attributes/asus-armoury/attributes/ppt_pl2_sppt/current_value")
            .unwrap();

        assert_eq!(spl_written, "20", "20000mW must translate to string '20'");
        assert_eq!(sppt_written, "25", "25000mW must translate to string '25'");
    }
}
