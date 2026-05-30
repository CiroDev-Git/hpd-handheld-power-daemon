// SPDX-License-Identifier: GPL-3.0-or-later

//! Temperature- and power-sensor telemetry capability.
//!
//! Read-only access to the platform's principal sensors (CPU/GPU
//! temperature and SoC power draw), used by status/monitor surfaces (and
//! by an operator validating that a fan curve actually cools). Separate
//! from [`fan::FanControl`](crate::fan::FanControl) (which reads fan RPM)
//! and [`fan_curve::FanCurveControl`](crate::fan_curve::FanCurveControl)
//! (which writes curves) because telemetry is a distinct concern with
//! its own optional-presence semantics.

use crate::units::{Celsius, PowerMilliwatts};
use hpd_error::HpdError;

/// Read-only access to CPU/GPU temperatures and SoC power draw.
pub trait ThermalSensors: Send + Sync {
    /// Current CPU/SoC package temperature, or `Ok(None)` when the
    /// platform exposes no readable CPU sensor.
    fn get_cpu_temp(&self) -> Result<Option<Celsius>, HpdError>;
    /// Current GPU temperature, or `Ok(None)` when the platform exposes
    /// no distinct GPU sensor.
    fn get_gpu_temp(&self) -> Result<Option<Celsius>, HpdError>;
    /// Current SoC / package power draw (the *actual* watts the chip is
    /// pulling, as opposed to the configured TDP limit), or `Ok(None)`
    /// when the platform exposes no readable power sensor. Default
    /// implementation returns `None` so backends without a power sensor
    /// need not implement it.
    fn get_soc_power(&self) -> Result<Option<PowerMilliwatts>, HpdError> {
        Ok(None)
    }
}
