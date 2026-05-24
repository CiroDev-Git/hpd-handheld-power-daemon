// SPDX-License-Identifier: GPL-3.0-or-later

//! Battery charge-control capability.

use crate::error::HpdError;

/// Minimum charge end threshold the daemon will accept. Lower values
/// would prevent some controllers from accepting writes at all.
pub const MIN_CHARGE_THRESHOLD: u8 = 20;
/// Maximum charge end threshold (100% = no cap).
pub const MAX_CHARGE_THRESHOLD: u8 = 100;
/// Initial charge-end-threshold used when no state has been persisted yet
/// and the backend cannot report the current value. 80 is the
/// long-battery-life sweet spot recommended by most cell vendors.
pub const DEFAULT_CHARGE_THRESHOLD: u8 = 80;

/// Read battery AC status and read/write the charge end threshold.
pub trait ChargeControl: Send + Sync {
    /// Returns whether AC is currently connected at the hardware level.
    fn is_ac_connected(&self) -> Result<bool, HpdError>;

    /// Writes the charge end threshold (percentage 0..=100). Callers are
    /// expected to clamp to `MIN_CHARGE_THRESHOLD..=MAX_CHARGE_THRESHOLD`
    /// before invoking.
    fn set_end_threshold(&self, threshold: u8) -> Result<(), HpdError>;

    /// Returns the current charge end threshold reported by the kernel.
    fn get_end_threshold(&self) -> Result<u8, HpdError>;
}
