// SPDX-License-Identifier: GPL-3.0-or-later

//! Aggregate backend trait that an L1 vendor implementation must satisfy.

use std::sync::Arc;

use crate::charge::ChargeControl;
use crate::fan::FanControl;
use crate::fan_curve::FanCurveControl;
use crate::gpu_clock::GpuClockRangeControl;
use crate::platform_profile::PlatformProfile;
use crate::power::PowerEnvelope;
use crate::telemetry::SystemTelemetry;
use crate::thermal::ThermalSensors;

/// Trait every L1 vendor backend implements to expose its capabilities
/// to the daemon. Each capability is reached through an explicit
/// accessor so a partial-capability vendor (e.g. Steam Deck has no
/// ACPI `platform_profile` rail, some boards have no dedicated GPU
/// fan sensor) can opt out by returning `None` from the matching
/// method.
///
/// [`HwBackend::power`] is mandatory because TDP control is the
/// daemon's reason for existing — a backend that cannot expose
/// [`PowerEnvelope`] should not be wired in at all. The other three
/// accessors default to `None` so adding a vendor with partial
/// hardware support is a *one-method* impl, not a four-method impl.
///
/// ## Stability
///
/// Part of the public Rust API surface **frozen for the 1.0 release**.
/// Adding a new capability is *additive* (a new accessor with a `None`
/// default and a corresponding new trait under `hpd_capabilities`).
/// Removing or renaming an existing accessor is a SemVer-major bump.
pub trait HwBackend: Send + Sync {
    /// Mandatory power-envelope accessor. Every backend must expose
    /// [`PowerEnvelope`]; absent it, the daemon has nothing to do.
    fn power(&self) -> &dyn PowerEnvelope;

    /// Optional battery charge-threshold accessor. Returns `None` on
    /// hardware that does not expose `charge_control_end_threshold`
    /// (or equivalent). Default: `None`.
    fn charge(&self) -> Option<&dyn ChargeControl> {
        None
    }

    /// Optional ACPI platform-profile accessor. Returns `None` on
    /// hardware that does not expose
    /// `/sys/firmware/acpi/platform_profile` (e.g. Steam Deck under
    /// the stock SteamOS kernel). Default: `None`.
    fn profile(&self) -> Option<&dyn PlatformProfile> {
        None
    }

    /// Optional fan-telemetry accessor. Returns `None` on hardware
    /// that does not expose hwmon fan inputs the daemon can read.
    /// Default: `None`.
    fn fan(&self) -> Option<&dyn FanControl> {
        None
    }

    /// Optional custom-fan-curve accessor. Returns `None` on hardware
    /// that does not expose an EC-mediated programmable fan curve (e.g.
    /// the `asus_custom_fan_curve` hwmon). Default: `None`.
    fn fan_curve(&self) -> Option<&dyn FanCurveControl> {
        None
    }

    /// Optional temperature-sensor accessor. Returns `None` on hardware
    /// that exposes no readable CPU/GPU temperature sensors. Default:
    /// `None`.
    fn thermal(&self) -> Option<&dyn ThermalSensors> {
        None
    }

    /// Optional extended-telemetry accessor (battery power/health,
    /// CPU/GPU clocks, GPU load, VRAM). Returns `None` on hardware that
    /// exposes none of it; individual fields are independently optional
    /// within the trait itself. Default: `None`.
    fn telemetry(&self) -> Option<&dyn SystemTelemetry> {
        None
    }

    /// Optional GPU clock-range accessor. Returns `None` on hardware
    /// that does not expose amdgpu's OverDrive `pp_od_clk_voltage`
    /// interface. Default: `None`.
    fn gpu_clock(&self) -> Option<&dyn GpuClockRangeControl> {
        None
    }
}

/// Forward every accessor through a shared [`Arc`]. Lets the daemon hold
/// the backend behind an `Arc<dyn HwBackend>` so the Executor (which
/// consumes it) and the D-Bus interface (which reads live telemetry) can
/// share one instance.
impl<T: HwBackend + ?Sized> HwBackend for Arc<T> {
    fn power(&self) -> &dyn PowerEnvelope {
        (**self).power()
    }
    fn charge(&self) -> Option<&dyn ChargeControl> {
        (**self).charge()
    }
    fn profile(&self) -> Option<&dyn PlatformProfile> {
        (**self).profile()
    }
    fn fan(&self) -> Option<&dyn FanControl> {
        (**self).fan()
    }
    fn fan_curve(&self) -> Option<&dyn FanCurveControl> {
        (**self).fan_curve()
    }
    fn thermal(&self) -> Option<&dyn ThermalSensors> {
        (**self).thermal()
    }
    fn telemetry(&self) -> Option<&dyn SystemTelemetry> {
        (**self).telemetry()
    }
    fn gpu_clock(&self) -> Option<&dyn GpuClockRangeControl> {
        (**self).gpu_clock()
    }
}

#[cfg(test)]
mod tests {
    // Test code may use `.unwrap()` / `.expect()` / `panic!` freely;
    // the strict bar in `[workspace.lints.clippy]` applies to
    // production code only.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use crate::power::{PowerEnvelopeLimits, PowerEnvelopeTarget};
    use crate::units::PowerMilliwatts;
    use hpd_error::HpdError;

    /// A vendor backend that only implements `PowerEnvelope` — no
    /// charge, no profile, no fan. Locks the partial-capability
    /// contract introduced by Lote 39 / Audit V2 §4.18.2: a future
    /// Steam Deck / handheld lacking some capability must be able to
    /// wire in without a `FeatureUnsupported`-everywhere shim.
    struct PowerOnlyBackend;

    impl PowerEnvelope for PowerOnlyBackend {
        fn get_limits(&self) -> Result<PowerEnvelopeLimits, HpdError> {
            Ok(PowerEnvelopeLimits {
                spl_min: PowerMilliwatts(7_000),
                spl_max: PowerMilliwatts(35_000),
                sppt_min: PowerMilliwatts(7_000),
                sppt_max: PowerMilliwatts(43_000),
                fppt_min: PowerMilliwatts(7_000),
                fppt_max: PowerMilliwatts(55_000),
            })
        }
        fn get_target(&self) -> Result<PowerEnvelopeTarget, HpdError> {
            Ok(PowerEnvelopeTarget {
                spl: PowerMilliwatts(15_000),
                sppt: PowerMilliwatts(17_000),
                fppt: Some(PowerMilliwatts(19_000)),
            })
        }
        fn set_target(&self, _target: &PowerEnvelopeTarget) -> Result<(), HpdError> {
            Ok(())
        }
    }

    impl HwBackend for PowerOnlyBackend {
        fn power(&self) -> &dyn PowerEnvelope {
            self
        }
        // charge / profile / fan default to None — the whole point.
    }

    #[test]
    fn power_only_backend_compiles_and_returns_none_for_optional_caps() {
        let b = PowerOnlyBackend;
        assert!(b.power().get_limits().is_ok());
        assert!(b.charge().is_none());
        assert!(b.profile().is_none());
        assert!(b.fan().is_none());
        assert!(b.fan_curve().is_none());
        assert!(b.gpu_clock().is_none());
    }
}
