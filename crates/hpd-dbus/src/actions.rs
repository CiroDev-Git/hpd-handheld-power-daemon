// SPDX-License-Identifier: GPL-3.0-or-later

//! Polkit action identifiers used by the D-Bus interface.
//!
//! Kept as a typed enum so the strings live in one place — adding a new
//! privileged operation now means adding a variant, getting a compile
//! error at the matching `as_id` arm, and updating the matching
//! `<action>` block in `package/polkit/dev.cirodev.hpd.policy`.

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
}

impl PolkitAction {
    pub const fn as_id(self) -> &'static str {
        match self {
            PolkitAction::SetTdp => "dev.cirodev.hpd.set-tdp",
            PolkitAction::SetCharge => "dev.cirodev.hpd.set-charge",
            PolkitAction::SetProfile => "dev.cirodev.hpd.set-profile",
        }
    }
}
