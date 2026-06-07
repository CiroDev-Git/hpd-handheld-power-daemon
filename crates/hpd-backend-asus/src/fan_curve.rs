// SPDX-License-Identifier: GPL-3.0-or-later

//! [`FanCurveControl`] implementation for ASUS handhelds.
//!
//! Writes the EC-mediated custom fan curve exposed by the
//! `asus_custom_fan_curve` hwmon node (`pwmN_auto_pointK_{temp,pwm}` +
//! `pwmN_enable`). `pwm1` drives the CPU/SoC fan, `pwm2` the GPU fan.
//!
//! ## Why the firmware default runs hot
//!
//! On the Xbox Ally X (RC73XA) the firmware's default `performance`
//! curve is only defined up to ~62 °C and tops out around 22 % duty;
//! the four high-temperature points are dead duplicates. Above 62 °C
//! the EC coasts conservatively, which is why the chip sits in the high
//! 80s °C under load. The presets below extend a monotonic curve out to
//! ~92 °C so the fans actually ramp where the firmware gives up.
//!
//! ## Preset calibration scope
//!
//! These tables are calibrated against the ROG Xbox Ally X (RC73XA),
//! the only model with an on-device sysfs capture so far. Other ASUS
//! handhelds (Ally, Ally X) share the same `asus_custom_fan_curve`
//! interface and so can use these curves safely (they are EC-mediated
//! auto-points, never raw PWM), but they are not yet tuned per-model —
//! that lands when captures from those units exist.

use hpd_capabilities::fan_curve::{
    ActiveFanCurves, FanCurve, FanCurveControl, FanCurvePoint, FanCurvePreset, FanCurveSelection,
    FAN_CURVE_POINTS,
};
use hpd_error::{BackendError, HpdError};
use hpd_sysfs::SysfsIo;

use crate::hwmon::find_hwmon_by_name;

/// hwmon `name` of the writable custom-fan-curve node.
const CURVE_HWMON_NAME: &str = "asus_custom_fan_curve";

/// `pwmN_enable` value that activates the manual/custom curve.
const ENABLE_CUSTOM: &str = "1";
/// `pwmN_enable` value that returns control to the firmware's automatic
/// curve.
const ENABLE_AUTO: &str = "2";

/// `pwm1` addresses the CPU/SoC fan, `pwm2` the GPU fan.
const FAN_CPU: u8 = 1;
const FAN_GPU: u8 = 2;

// Calibration note (Xbox Ally X / RC73XA, validated in-game June 2026):
// the EC enforces a hard fan floor of ~3700 RPM (any duty <= ~8% still
// spins at the floor) and a ceiling of ~8400 RPM under real thermal
// demand. RPM saturates fast — most of the floor->ceiling span is
// covered by duty ~20..220, and duty above ~220 buys almost no extra
// airflow. The curves below therefore stop chasing the bottom (the fan
// can't run quieter than the floor) and instead reach near-max airflow
// EARLY in temperature, biasing toward cooling.

/// Quietest preset (pairs with the PowerSaver profile, which already
/// throttles the APU to ~13 W, so the chip stays cool on its own). Sits
/// near the fan floor through the low/mid range, then ramps firmly past
/// ~78 °C so it never relies on the firmware's undefined high-temp region.
const SILENT: FanCurve = FanCurve::new([
    FanCurvePoint::new(50, 15),
    FanCurvePoint::new(58, 28),
    FanCurvePoint::new(65, 55),
    FanCurvePoint::new(72, 95),
    FanCurvePoint::new(78, 150),
    FanCurvePoint::new(83, 200),
    FanCurvePoint::new(88, 235),
    FanCurvePoint::new(93, 255),
]);

/// Default after install: noticeably cooler than firmware. Holds the
/// chip in the low 60s °C at the Balanced profile's ~17-21 W in-game,
/// reaching full duty by ~85 °C.
const BALANCED: FanCurve = FanCurve::new([
    FanCurvePoint::new(45, 20),
    FanCurvePoint::new(54, 50),
    FanCurvePoint::new(62, 95),
    FanCurvePoint::new(69, 145),
    FanCurvePoint::new(75, 190),
    FanCurvePoint::new(80, 225),
    FanCurvePoint::new(85, 255),
    FanCurvePoint::new(92, 255),
]);

/// Cooling-first, Armoury-Crate "Turbo" style: near-max airflow by
/// ~74 °C to keep the screen and back panel cool, at the cost of noise.
/// Validated holding ~78 °C under a sustained 40 W (Performance) game
/// load with the fans pinned at the ~8000 RPM ceiling.
const AGGRESSIVE: FanCurve = FanCurve::new([
    FanCurvePoint::new(40, 45),
    FanCurvePoint::new(48, 90),
    FanCurvePoint::new(55, 135),
    FanCurvePoint::new(62, 180),
    FanCurvePoint::new(68, 220),
    FanCurvePoint::new(74, 255),
    FanCurvePoint::new(82, 255),
    FanCurvePoint::new(90, 255),
]);

/// Resolve a named preset to its `(cpu, gpu)` curves. Both fans share
/// the same temperature→duty mapping; because the GPU runs cooler than
/// the CPU under the same load, its fan naturally spins less. The EC
/// evaluates `pwm1` against CPU temperature and `pwm2` against GPU
/// temperature independently.
const fn preset_curves(preset: FanCurvePreset) -> (FanCurve, FanCurve) {
    let curve = match preset {
        FanCurvePreset::Silent => SILENT,
        FanCurvePreset::Balanced => BALANCED,
        FanCurvePreset::Aggressive => AGGRESSIVE,
    };
    (curve, curve)
}

/// [`FanCurveControl`] implementation for ASUS handhelds.
///
/// Locates the `asus_custom_fan_curve` hwmon by `name` (never by index —
/// see the crate's `hwmon` module) and programs the eight auto-points
/// per fan, then reads them back to confirm the EC accepted the write.
pub struct AsusFanCurveBackend<S: SysfsIo> {
    sysfs: S,
}

impl<S: SysfsIo> AsusFanCurveBackend<S> {
    /// Wrap a `SysfsIo` handle (see [`AsusBackend::new`](crate::AsusBackend::new)).
    pub fn new(sysfs: S) -> Self {
        Self { sysfs }
    }

    /// Resolve the curve hwmon base path, or [`HpdError::FeatureUnsupported`]
    /// when the node is absent (kernel without custom-fan-curve support).
    fn curve_base(&self) -> Result<String, HpdError> {
        find_hwmon_by_name(&self.sysfs, CURVE_HWMON_NAME).ok_or(HpdError::FeatureUnsupported)
    }

    fn point_path(base: &str, fan: u8, point: usize, kind: &str) -> String {
        format!("{base}/pwm{fan}_auto_point{point}_{kind}")
    }

    fn enable_path(base: &str, fan: u8) -> String {
        format!("{base}/pwm{fan}_enable")
    }

    /// Write the eight `(temp, pwm)` points for one fan, in point order.
    /// The EC requires monotonic temperatures; callers pass validated
    /// curves.
    fn write_fan_points(&self, base: &str, fan: u8, curve: &FanCurve) -> Result<(), HpdError> {
        for (i, p) in curve.points.iter().enumerate() {
            let point = i + 1;
            self.sysfs.write_string(
                Self::point_path(base, fan, point, "temp"),
                &p.temp_c.to_string(),
            )?;
            self.sysfs.write_string(
                Self::point_path(base, fan, point, "pwm"),
                &p.pwm.to_string(),
            )?;
        }
        Ok(())
    }

    /// Read back the eight points for one fan.
    fn read_fan_points(&self, base: &str, fan: u8) -> Result<FanCurve, HpdError> {
        let mut points = [FanCurvePoint::new(0, 0); FAN_CURVE_POINTS];
        for (i, slot) in points.iter_mut().enumerate() {
            let point = i + 1;
            let temp = self.read_u8(&Self::point_path(base, fan, point, "temp"))?;
            let pwm = self.read_u8(&Self::point_path(base, fan, point, "pwm"))?;
            *slot = FanCurvePoint::new(temp, pwm);
        }
        Ok(FanCurve::new(points))
    }

    fn read_u8(&self, path: &str) -> Result<u8, HpdError> {
        let raw = self.sysfs.read_string(path)?;
        raw.trim().parse::<u8>().map_err(|_| {
            HpdError::Backend(BackendError::ParseFailed {
                field: "fan_curve_point",
                raw: raw.clone(),
                reason: "expected u8 (0-255)".into(),
            })
        })
    }

    /// Resolve a selection to concrete `(cpu, gpu)` curves, validating
    /// caller-supplied custom curves at this trust boundary.
    fn resolve(selection: &FanCurveSelection) -> Result<(FanCurve, FanCurve), HpdError> {
        match selection {
            FanCurveSelection::Preset(preset) => Ok(preset_curves(*preset)),
            FanCurveSelection::Custom { cpu, gpu } => {
                cpu.validate()?;
                gpu.validate()?;
                Ok((*cpu, *gpu))
            }
        }
    }
}

impl<S: SysfsIo> FanCurveControl for AsusFanCurveBackend<S> {
    fn apply(&self, selection: &FanCurveSelection) -> Result<(), HpdError> {
        let (cpu, gpu) = Self::resolve(selection)?;
        let base = self.curve_base()?;

        self.write_fan_points(&base, FAN_CPU, &cpu)?;
        self.write_fan_points(&base, FAN_GPU, &gpu)?;

        // Switch the EC to custom-curve mode after the points are in
        // place, so it never runs a half-written curve.
        self.sysfs
            .write_string(Self::enable_path(&base, FAN_CPU), ENABLE_CUSTOM)?;
        self.sysfs
            .write_string(Self::enable_path(&base, FAN_GPU), ENABLE_CUSTOM)?;

        // Read back and fail closed if the EC did not store what we
        // asked for — a silently-rejected curve must not look like
        // success to the daemon.
        let applied = self.get_curves()?;
        if applied.cpu != cpu || applied.gpu != gpu {
            return Err(HpdError::Backend(BackendError::Other(format!(
                "fan curve read-back mismatch: wrote cpu={:?} gpu={:?}, EC reports cpu={:?} gpu={:?}",
                cpu.points, gpu.points, applied.cpu.points, applied.gpu.points
            ))));
        }
        Ok(())
    }

    fn reset_to_auto(&self) -> Result<(), HpdError> {
        let base = self.curve_base()?;
        self.sysfs
            .write_string(Self::enable_path(&base, FAN_CPU), ENABLE_AUTO)?;
        self.sysfs
            .write_string(Self::enable_path(&base, FAN_GPU), ENABLE_AUTO)?;
        Ok(())
    }

    fn get_curves(&self) -> Result<ActiveFanCurves, HpdError> {
        let base = self.curve_base()?;
        Ok(ActiveFanCurves {
            cpu: self.read_fan_points(&base, FAN_CPU)?,
            gpu: self.read_fan_points(&base, FAN_GPU)?,
        })
    }

    fn active_selection(&self) -> Result<Option<FanCurveSelection>, HpdError> {
        let base = self.curve_base()?;
        // Firmware-auto mode → no custom curve is in charge.
        if self
            .sysfs
            .read_string(Self::enable_path(&base, FAN_CPU))?
            .trim()
            == ENABLE_AUTO
        {
            return Ok(None);
        }
        // Custom mode: match the live points against each known preset so
        // the daemon reports the real level, falling back to `Custom`.
        let curves = self.get_curves()?;
        for preset in [
            FanCurvePreset::Silent,
            FanCurvePreset::Balanced,
            FanCurvePreset::Aggressive,
        ] {
            let (cpu, gpu) = preset_curves(preset);
            if curves.cpu == cpu && curves.gpu == gpu {
                return Ok(Some(FanCurveSelection::Preset(preset)));
            }
        }
        Ok(Some(FanCurveSelection::Custom {
            cpu: curves.cpu,
            gpu: curves.gpu,
        }))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use hpd_sysfs::MockSysfs;

    /// Seed a mock `asus_custom_fan_curve` node with all 16 point files
    /// and both enable files so writes/read-backs round-trip.
    fn seed_curve_node(mock: &MockSysfs) {
        mock.create_file("sys/class/hwmon/hwmon8/name", "asus_custom_fan_curve");
        for fan in [FAN_CPU, FAN_GPU] {
            mock.create_file(format!("sys/class/hwmon/hwmon8/pwm{fan}_enable"), "2");
            for point in 1..=FAN_CURVE_POINTS {
                mock.create_file(
                    format!("sys/class/hwmon/hwmon8/pwm{fan}_auto_point{point}_temp"),
                    "0",
                );
                mock.create_file(
                    format!("sys/class/hwmon/hwmon8/pwm{fan}_auto_point{point}_pwm"),
                    "0",
                );
            }
        }
    }

    #[test]
    fn apply_preset_writes_points_and_enables_custom() {
        let mock = MockSysfs::new();
        seed_curve_node(&mock);
        let backend = AsusFanCurveBackend::new(mock.clone());

        backend
            .apply(&FanCurveSelection::Preset(FanCurvePreset::Balanced))
            .expect("apply must succeed");

        // Point 8 of the CPU fan should be the balanced top point.
        assert_eq!(
            mock.read_string("/sys/class/hwmon/hwmon8/pwm1_auto_point8_temp")
                .unwrap(),
            "92"
        );
        assert_eq!(
            mock.read_string("/sys/class/hwmon/hwmon8/pwm1_auto_point8_pwm")
                .unwrap(),
            "255"
        );
        // Both fans switched to custom mode.
        assert_eq!(
            mock.read_string("/sys/class/hwmon/hwmon8/pwm1_enable")
                .unwrap(),
            ENABLE_CUSTOM
        );
        assert_eq!(
            mock.read_string("/sys/class/hwmon/hwmon8/pwm2_enable")
                .unwrap(),
            ENABLE_CUSTOM
        );
    }

    #[test]
    fn active_selection_reflects_the_live_ec() {
        let mock = MockSysfs::new();
        seed_curve_node(&mock); // enable seeded as "2" (firmware auto)
        let backend = AsusFanCurveBackend::new(mock.clone());

        // Firmware-auto mode → no custom selection.
        assert_eq!(backend.active_selection().unwrap(), None);

        // After applying a preset, it reads back as that exact preset.
        backend
            .apply(&FanCurveSelection::Preset(FanCurvePreset::Aggressive))
            .unwrap();
        assert_eq!(
            backend.active_selection().unwrap(),
            Some(FanCurveSelection::Preset(FanCurvePreset::Aggressive))
        );

        // Custom mode with points matching no preset → Custom (never a
        // misleading preset name).
        mock.write_string("/sys/class/hwmon/hwmon8/pwm1_auto_point8_pwm", "200")
            .unwrap();
        assert!(matches!(
            backend.active_selection().unwrap(),
            Some(FanCurveSelection::Custom { .. })
        ));
    }

    #[test]
    fn get_curves_reads_back_what_apply_wrote() {
        let mock = MockSysfs::new();
        seed_curve_node(&mock);
        let backend = AsusFanCurveBackend::new(mock.clone());

        backend
            .apply(&FanCurveSelection::Preset(FanCurvePreset::Aggressive))
            .expect("apply must succeed");

        let active = backend.get_curves().expect("read-back must succeed");
        assert_eq!(active.cpu, AGGRESSIVE);
        assert_eq!(active.gpu, AGGRESSIVE);
    }

    #[test]
    fn custom_selection_is_validated() {
        let mock = MockSysfs::new();
        seed_curve_node(&mock);
        let backend = AsusFanCurveBackend::new(mock.clone());

        // Non-monotonic temperature must be rejected before any write.
        let mut bad = BALANCED;
        bad.points[4] = FanCurvePoint::new(10, 140);
        let err = backend.apply(&FanCurveSelection::Custom {
            cpu: bad,
            gpu: BALANCED,
        });
        assert!(err.is_err());
    }

    #[test]
    fn reset_to_auto_sets_enable_two() {
        let mock = MockSysfs::new();
        seed_curve_node(&mock);
        let backend = AsusFanCurveBackend::new(mock.clone());

        backend
            .apply(&FanCurveSelection::Preset(FanCurvePreset::Silent))
            .unwrap();
        backend.reset_to_auto().expect("reset must succeed");

        assert_eq!(
            mock.read_string("/sys/class/hwmon/hwmon8/pwm1_enable")
                .unwrap(),
            ENABLE_AUTO
        );
        assert_eq!(
            mock.read_string("/sys/class/hwmon/hwmon8/pwm2_enable")
                .unwrap(),
            ENABLE_AUTO
        );
    }

    #[test]
    fn missing_curve_node_is_feature_unsupported() {
        let mock = MockSysfs::new();
        let backend = AsusFanCurveBackend::new(mock.clone());
        assert!(matches!(
            backend.apply(&FanCurveSelection::Preset(FanCurvePreset::Balanced)),
            Err(HpdError::FeatureUnsupported)
        ));
    }
}
