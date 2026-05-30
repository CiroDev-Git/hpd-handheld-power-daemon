// SPDX-License-Identifier: GPL-3.0-or-later

//! Temperature-sensor telemetry capability.
//!
//! Read-only access to the platform's principal temperature sensors,
//! used by status/monitor surfaces (and by an operator validating that
//! a fan curve actually cools). Separate from
//! [`fan::FanControl`](crate::fan::FanControl) (which reads fan RPM) and
//! [`fan_curve::FanCurveControl`](crate::fan_curve::FanCurveControl)
//! (which writes curves) because temperature is a distinct concern with
//! its own optional-presence semantics.

use crate::units::Celsius;
use hpd_error::HpdError;

/// Read-only access to CPU/GPU temperatures.
pub trait ThermalSensors: Send + Sync {
    /// Current CPU/SoC package temperature, or `Ok(None)` when the
    /// platform exposes no readable CPU sensor.
    fn get_cpu_temp(&self) -> Result<Option<Celsius>, HpdError>;
    /// Current GPU temperature, or `Ok(None)` when the platform exposes
    /// no distinct GPU sensor.
    fn get_gpu_temp(&self) -> Result<Option<Celsius>, HpdError>;
}
