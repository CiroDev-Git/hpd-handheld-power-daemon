// SPDX-License-Identifier: GPL-3.0-or-later

//! Fan telemetry capability.

use crate::units::Rpm;
use hpd_error::HpdError;

/// Read-only access to fan RPM readings.
pub trait FanControl: Send + Sync {
    /// Returns the current CPU fan RPM.
    fn get_cpu_fan_rpm(&self) -> Result<Rpm, HpdError>;
    /// Returns the current GPU fan RPM if the hardware exposes a
    /// distinct GPU fan; `Ok(None)` when the platform has a single fan.
    fn get_gpu_fan_rpm(&self) -> Result<Option<Rpm>, HpdError>;
}
