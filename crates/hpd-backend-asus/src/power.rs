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
        let val_str = self.sysfs.read_string(&path)?;

        let watts: u32 = val_str.parse().map_err(|_| HpdError::Backend {
            reason: format!("Failed to parse integer from {}", path),
        })?;

        // Convertion of W (kernel) a mW (domain)
        Ok(PowerMilliwatts(watts * 1000))
    }

    fn write_watts(&self, attr: &str, target_mw: PowerMilliwatts) -> Result<(), HpdError> {
        let path = format!("{}/{}/current_value", BASE_PATH, attr);
        // Convertion of mW (domain) a W (kernel)
        let watts = target_mw.0 / 1000;
        self.sysfs.write_string(&path, &watts.to_string())?;
        Ok(())
    }
}

impl<S: SysfsIo> PowerEnvelope for AsusPowerBackend<S> {
    fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError> {
        let min = self.read_watts("ppt_pl1_spl", "min_value")?;
        let max = self.read_watts("ppt_pl1_spl", "max_value")?;

        Ok(PowerEnvelopeLimits {
            spl_min: min,
            spl_max: max,
        })
    }

    fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError> {
        let spl = self.read_watts("ppt_pl1_spl", "current_value")?;
        let sppt = self.read_watts("ppt_pl2_sppt", "current_value")?;
        let fppt = self.read_watts("ppt_pl3_fppt", "current_value")?; // notar el pl3_

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
