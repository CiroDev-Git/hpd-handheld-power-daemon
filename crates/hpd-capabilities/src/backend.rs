//! Aggregate backend trait that an L1 vendor implementation must satisfy.

use crate::charge::ChargeControl;
use crate::fan::FanControl;
use crate::platform_profile::PlatformProfile;
use crate::power::PowerEnvelope;

/// Blanket trait that bundles every L2 capability. Each L1 vendor
/// crate (ASUS, Lenovo, Valve, …) implements the four underlying
/// traits and gets `HwBackend` for free via the empty impl block.
pub trait HwBackend: PowerEnvelope + ChargeControl + PlatformProfile + FanControl + Send + Sync {}