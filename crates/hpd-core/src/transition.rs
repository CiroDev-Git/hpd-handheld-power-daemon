// SPDX-License-Identifier: GPL-3.0-or-later

//! Every external event that can mutate the daemon's state goes
//! through one of these variants.

use hpd_capabilities::fan_curve::{FanCurvePreset, FanCurveSelection};
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
    /// Cooling lever (fans only): set the **fan curve** to the requested
    /// preset (`Silent` / `Balanced` / `Aggressive`) and disable
    /// `fan_follows_tdp` (manual cooling). Decoupled from power — it does
    /// **not** touch the ACPI `platform_profile` or the power envelope.
    /// This is the front-end for `hpdctl cool set`; the `SetProfile`
    /// transition is the separate power-profile lever.
    SetCoolingLevel(FanCurvePreset),
    /// User-requested change of the battery charge end threshold.
    ChargeThresholdChanged(u8),
    /// Forced rollback to the power envelope the kernel actually
    /// reports, used by the executor after `set_target` fails.
    SyncPowerTarget(PowerEnvelopeTarget),
    /// Forced rollback to the platform profile the kernel actually
    /// reports, used by the executor after `set_active_profile` fails.
    SyncPlatformProfile(ProfileName),
    /// Forced rollback to the charge end threshold the kernel actually
    /// reports, used by the executor after `set_end_threshold` fails.
    SyncChargeThreshold(u8),
    /// Forced rollback to the fan-curve selection the EC actually runs
    /// (`None` = firmware auto), used by the executor after `apply` /
    /// `reset_to_auto` fails, so the reported level never lies.
    SyncFanCurve(Option<FanCurveSelection>),
    /// AC charger was plugged (`true`) or unplugged (`false`).
    /// Triggers preset swap + `last_dc_target` bookkeeping.
    AcPowerChanged(bool),
    /// System resumed from suspend; re-apply the persisted envelope,
    /// profile and charge threshold so the kernel sees the daemon's
    /// view of the world again.
    SystemResumed,
    /// Toggle the **"lock to maximum performance on AC"** preference
    /// (`ProfileState::ac_max_performance`). Persisted. Applied immediately:
    /// turning it **on** while plugged in snapshots the current state and
    /// forces Performance / Max / Aggressive + lock; turning it **off** while
    /// plugged in restores the battery snapshot (if any) and unlocks. On
    /// battery it just stores the preference. Never gated by the lock — it is
    /// how you *release* the lock.
    SetAcMaxPerformance(bool),
    /// Re-bind cooling-profile inference to the TDP envelope.
    EnableFanAuto,
    /// Hand fan control back to the firmware's automatic curve.
    ResetFanCurve,
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
