// SPDX-License-Identifier: GPL-3.0-or-later

//! Polkit action identifiers used by the D-Bus interface.
//!
//! Kept as a typed enum so the strings live in one place — adding a new
//! privileged operation now means adding a variant, getting a compile
//! error at the matching `as_id` arm, and updating the matching
//! `<action>` block in `package/polkit/dev.cirodev.hpd.policy`.
//!
//! The `auth_admin` / `auth_admin_keep` defaults noted below apply to
//! **non-administrator** callers. `wheel`-group members are granted
//! every `dev.cirodev.hpd.*` action without a prompt by
//! `package/polkit/49-hpd.rules`, which matches by action-ID prefix —
//! so a new variant is covered by that grant automatically.

/// Typed identifier for every polkit action the daemon registers.
///
/// One variant per privileged D-Bus setter. The mapping to the
/// `action_id` strings declared in
/// `package/polkit/dev.cirodev.hpd.policy` lives in [`Self::as_id`];
/// adding a new variant triggers a compile error there until the
/// matching policy block is wired up.
#[derive(Debug, Clone, Copy)]
pub enum PolkitAction {
    /// Change the TDP envelope (SPL / SPPT / FPPT or a preset).
    /// High-impact: `auth_admin` (prompt on every call).
    SetTdp,
    /// Change the battery charge end threshold.
    /// High-impact: `auth_admin`.
    SetCharge,
    /// Change the platform cooling profile or re-bind fan-auto.
    /// Low-impact, cosmetic-leaning: `auth_admin_keep` (5 min cache).
    SetProfile,
    /// Program or reset the EC-mediated custom fan curve.
    /// Low-impact, cosmetic-leaning: `auth_admin_keep` (5 min cache).
    SetFanCurve,
}

impl PolkitAction {
    /// Polkit `action_id` string declared in the project's policy file.
    pub const fn as_id(self) -> &'static str {
        match self {
            PolkitAction::SetTdp => "dev.cirodev.hpd.set-tdp",
            PolkitAction::SetCharge => "dev.cirodev.hpd.set-charge",
            PolkitAction::SetProfile => "dev.cirodev.hpd.set-profile",
            PolkitAction::SetFanCurve => "dev.cirodev.hpd.set-fan-curve",
        }
    }
}
