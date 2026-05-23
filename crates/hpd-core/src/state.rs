//! Persistent state of the daemon.

use serde::{Deserialize, Serialize};
use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::ProfileName;

/// Immutable snapshot of everything the L3 executor needs to know
/// across transitions and across reboots. Wrapped in a
/// `tokio::sync::watch` channel and serialised to TOML on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileState {
    /// Currently programmed power envelope (SPL / SPPT / FPPT).
    pub power_target: PowerEnvelopeTarget,
    /// Active ACPI platform / cooling profile.
    pub active_profile: ProfileName,
    /// Battery charge end threshold (percentage 20..=100).
    pub charge_end_threshold: u8,
    /// When `true`, every TDP change re-infers and applies a matching
    /// platform profile; explicit `set_profile` calls flip it off until
    /// `EnableFanAuto` is sent.
    pub fan_follows_tdp: bool,
    /// Last envelope used while running on battery, restored on AC
    /// unplug. `None` until the first AC plug event mutates it.
    pub last_dc_target: Option<PowerEnvelopeTarget>,

    /// Whether AC is currently connected. Skipped during
    /// (de)serialisation — at boot we always re-query the backend
    /// rather than trusting a possibly-stale value from disk.
    #[serde(skip)]
    pub is_ac_connected: bool,
}
