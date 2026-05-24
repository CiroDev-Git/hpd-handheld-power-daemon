// SPDX-License-Identifier: GPL-3.0-or-later

//! Every external event that can mutate the daemon's state goes
//! through one of these variants.

use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::{ProfileName, RuntimeConfig, TdpPreset};

/// External event capable of altering [`crate::state::ProfileState`] or
/// the executor's runtime config.
#[derive(Debug, Clone)]
pub enum Transition {
    /// Apply a TDP preset (Eco / Balanced / Max) relative to the
    /// hardware SPL range.
    SetPreset(TdpPreset),
    /// Smart-mode SPL change in whole watts. Derives SPPT and FPPT via
    /// [`RuntimeConfig::sppt_factor`] / `fppt_factor`.
    SetSpl(u32),
    /// Manual mode: caller supplies every rail of the envelope.
    SetEnvelope(PowerEnvelopeTarget),
    /// Set the ACPI platform / cooling profile and disable
    /// `fan_follows_tdp` until the next `EnableFanAuto`.
    SetProfile(ProfileName),
    /// User-requested change of the battery charge end threshold.
    ChargeThresholdChanged(u8),
    /// Forced rollback to the value the kernel actually reports, used
    /// by the executor after a backend write fails.
    SyncPowerTarget(PowerEnvelopeTarget),
    /// AC charger was plugged (`true`) or unplugged (`false`).
    /// Triggers preset swap + `last_dc_target` bookkeeping.
    AcPowerChanged(bool),
    /// System resumed from suspend; re-apply the persisted envelope,
    /// profile and charge threshold so the kernel sees the daemon's
    /// view of the world again.
    SystemResumed,
    /// Re-bind cooling-profile inference to the TDP envelope.
    EnableFanAuto,
    /// Hot-reload of runtime-tunable config. Intercepted by the Executor
    /// before `reduce()` is called: the executor swaps its own
    /// `RuntimeConfig` and the next transition uses the new values. The
    /// reducer treats it as a no-op so calling `reduce()` with this
    /// variant in isolation (e.g. in unit tests) is harmless.
    ConfigReload(RuntimeConfig),
    /// Daemon is shutting down (SIGINT/SIGTERM received). The reducer
    /// emits `Effect::PersistState` so the in-memory state hits disk
    /// before the process exits; the Executor breaks its `run()` loop
    /// after dispatching the resulting effects.
    Shutdown,
}
