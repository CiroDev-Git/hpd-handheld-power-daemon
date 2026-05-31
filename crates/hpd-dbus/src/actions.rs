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
    /// Every action the daemon registers, in declaration order.
    ///
    /// Used by the startup self-check and the `get_diagnostics` D-Bus
    /// method to verify each action is registered with polkit. The
    /// exhaustive match in this module's tests turns a newly-added enum
    /// variant into a compile error here, prompting whoever adds it to
    /// extend this array too.
    pub const ALL: [PolkitAction; 4] = [
        PolkitAction::SetTdp,
        PolkitAction::SetCharge,
        PolkitAction::SetProfile,
        PolkitAction::SetFanCurve,
    ];

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_ids_are_unique_and_namespaced() {
        let ids: Vec<&str> = PolkitAction::ALL.iter().map(|a| a.as_id()).collect();
        for id in &ids {
            assert!(
                id.starts_with("dev.cirodev.hpd."),
                "action id {id} is outside the dev.cirodev.hpd.* namespace covered by 49-hpd.rules"
            );
        }
        let unique: std::collections::HashSet<&&str> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "duplicate action id in PolkitAction::ALL");
    }

    #[test]
    fn all_entries_match_a_known_variant() {
        // The exhaustive match makes adding a new enum variant a compile
        // error here, which is the prompt to also add it to ALL above.
        for action in PolkitAction::ALL {
            match action {
                PolkitAction::SetTdp
                | PolkitAction::SetCharge
                | PolkitAction::SetProfile
                | PolkitAction::SetFanCurve => {}
            }
        }
    }
}
