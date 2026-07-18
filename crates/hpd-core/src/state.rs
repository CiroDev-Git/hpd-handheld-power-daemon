// SPDX-License-Identifier: GPL-3.0-or-later

//! Persistent state of the daemon.

use hpd_capabilities::fan_curve::FanCurveSelection;
use hpd_capabilities::gpu_clock::GpuClockSelection;
use hpd_capabilities::power::PowerEnvelopeTarget;
use hpd_capabilities::profile::ProfileName;
use serde::{Deserialize, Serialize};

/// Serde default for [`ProfileState::ac_max_performance`]: the lock is on
/// unless a `state.toml` explicitly stored `false`.
fn default_ac_max_performance() -> bool {
    true
}

/// Snapshot of the user's **battery (DC)** power + cooling preferences,
/// captured the moment AC is plugged in and restored verbatim on unplug.
///
/// It exists so the "AC = maximum performance" policy can override every
/// power/cooling lever while plugged and still bring the user's own choices
/// back when they unplug. Persisted (not `#[serde(skip)]`) so the restore
/// survives a reboot taken while on AC. Replaces the old TDP-only
/// `last_dc_target`, which could only remember the envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DcSnapshot {
    /// Power envelope (SPL / SPPT / FPPT) the user ran on battery.
    pub power_target: PowerEnvelopeTarget,
    /// ACPI platform profile (Power mode) the user ran on battery.
    pub active_profile: ProfileName,
    /// Fan-curve selection the user ran on battery (`None` = firmware auto).
    #[serde(default)]
    pub active_fan_curve: Option<FanCurveSelection>,
    /// Whether auto-cooling (fan curve follows TDP) was on, on battery.
    pub fan_follows_tdp: bool,
    /// GPU clock-range selection the user ran on battery (`None` = firmware
    /// auto — the default for anyone who never opts in, see
    /// [`ProfileState::active_gpu_clock`]).
    #[serde(default)]
    pub active_gpu_clock: Option<GpuClockSelection>,
    /// Whether GPU-clock auto-follow was on, on battery.
    #[serde(default)]
    pub gpu_follows_tdp: bool,
}

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
    /// **fan curve** (cooling follows power; the ACPI `platform_profile`
    /// is a separate, decoupled lever and is never inferred here). Flipped
    /// off by an explicit `set_cooling_level` (manual cooling) and back on
    /// by `EnableFanAuto`. A manual `set_profile` does **not** touch it —
    /// power and cooling are independent.
    pub fan_follows_tdp: bool,
    /// The user's battery (DC) power + cooling preferences, snapshotted on
    /// the battery→AC edge and restored on unplug. `None` until the first
    /// AC plug event captures it. See [`DcSnapshot`].
    #[serde(default)]
    pub last_dc_state: Option<DcSnapshot>,

    /// **The "lock to maximum performance on AC" preference** (toggleable at
    /// runtime via `set_ac_max_performance`, persisted so it survives a
    /// reboot). When `true` (the default), plugging in pins **Performance /
    /// Max TDP / Aggressive** and rejects power/cooling writes until unplug;
    /// the battery (DC) prefs are restored on unplug. When `false`, AC is
    /// fully manual — plugging/unplugging changes nothing and everything
    /// stays editable. Seeded on first boot from
    /// `DaemonConfig::default_ac_max_performance`. `#[serde(default = …)]`
    /// to `true` so a `state.toml` predating this field loads as "locked".
    #[serde(default = "default_ac_max_performance")]
    pub ac_max_performance: bool,

    /// Active custom fan-curve selection. `None` means the firmware's
    /// automatic curve is in charge (the daemon is not managing the fan
    /// curve). Re-applied on resume so a suspend/resume cycle never
    /// leaves the EC running a stale or maxed-out curve. Defaults to
    /// `None` so state files written before this field existed load
    /// cleanly as "firmware auto".
    #[serde(default)]
    pub active_fan_curve: Option<FanCurveSelection>,

    /// Active GPU clock-range selection. `None` means firmware auto (the
    /// daemon never touches `power_dpm_force_performance_level`/
    /// `pp_od_clk_voltage`) — the **permanent default** for anyone who
    /// never opts in via `EnableGpuAutoFollow`, unlike `active_fan_curve`
    /// (whose real steady-state is never `None`). Every site that
    /// unconditionally re-pins/reapplies the fan curve today
    /// (`force_ac_max_performance`, the AC-plug-restore branch,
    /// `SystemResumed`'s reapply) must guard the matching GPU-clock effect
    /// on `active_gpu_clock.is_some()` — mirroring those sites
    /// unconditionally would silently auto-opt every user in the first
    /// time they plug in AC. `Some(Unmanaged(_))` is a rollback-only state
    /// no transition sets directly — see [`GpuClockSelection`]'s docs.
    #[serde(default)]
    pub active_gpu_clock: Option<GpuClockSelection>,

    /// When `true`, every TDP change re-infers and applies a matching GPU
    /// clock ceiling (mirrors `fan_follows_tdp`, but defaults to `false` —
    /// see `active_gpu_clock`'s docs on why the default differs from the
    /// fan curve). Flipped on by `EnableGpuAutoFollow`, off by
    /// `ResetGpuClocks`.
    #[serde(default)]
    pub gpu_follows_tdp: bool,

    /// Whether AC is currently connected. Skipped during
    /// (de)serialisation — at boot we always re-query the backend
    /// rather than trusting a possibly-stale value from disk.
    #[serde(skip)]
    pub is_ac_connected: bool,

    /// **Derived, never persisted.** `true` when the daemon is pinning every
    /// power/cooling lever to maximum performance because it is on AC and the
    /// `ac_max_performance` preference is enabled — in which case the reducer
    /// (and the D-Bus setters) reject user power/cooling writes (the battery
    /// charge threshold stays editable). The executor recomputes it on every
    /// state publish (`is_ac_connected && ac_max_performance`); it is surfaced
    /// over D-Bus as `AcLocked` so clients can disable their controls.
    /// `#[serde(skip)]` because it is a pure function of `is_ac_connected`
    /// (re-read at boot) and the persisted `ac_max_performance` preference.
    #[serde(skip)]
    pub ac_locked: bool,
}
