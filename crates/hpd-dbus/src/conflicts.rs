// SPDX-License-Identifier: GPL-3.0-or-later

//! Competing power-daemon detection.
//!
//! `hpd` expects to be the sole manager of the platform's power knobs
//! (TDP via the ASUS firmware attributes, the ACPI platform profile, the
//! EC fan curve, the battery charge threshold). Other system power
//! daemons write the *same* sysfs/ACPI surfaces, so running one alongside
//! `hpd` makes the last writer win and the effective state flap.
//!
//! This module only *detects* — it never stops anything: the daemon is
//! sandboxed (`ProtectSystem=strict`) and cannot, and silently disabling
//! another package's service is the wrong layer. The repair is the
//! user-side `hpdctl doctor --fix`, mirroring the polkit split where the
//! daemon detects ([`crate::polkit::missing_actions`]) and the CLI repairs
//! ([`hpdctl fix-polkit`]).
//!
//! ## Two axes: hard-vs-advisory, and how each is detected
//!
//! **Hard rivals** own the *same* knobs hpd does and must not co-run —
//! `hpdctl doctor --fix` masks them. **Advisory** daemons only *touch*
//! adjacent surfaces (mostly the CPU governor) and are legitimately wanted,
//! so hpd reports them but `doctor --fix` never masks them. Keeping the two
//! apart is what stops the repair from killing, say, GameMode out from
//! under a running game, or asusd's keyboard RGB.
//!
//! Orthogonally, a rival is detected one of two ways:
//!
//! * **By D-Bus name** ([`RIVAL_POWER_DAEMONS`], [`ADVISORY_POWER_DAEMONS`])
//!   — `org.freedesktop.DBus.NameHasOwner`, which reports current ownership
//!   without D-Bus-activating the service, so checking never revives a
//!   masked rival.
//! * **By active systemd unit** ([`RIVAL_UNITS`], [`ADVISORY_UNITS`]) — for
//!   daemons that own no well-known bus name (Handheld Daemon,
//!   auto-cpufreq), `org.freedesktop.systemd1`'s `ListUnitsByPatterns`.
//!
//! The wild handheld images motivate every entry: SteamOS Game Mode ships
//! `steamos-manager` (+ `gamescope`, + optional `gamemoded`); GNOME/KDE
//! desktops ship `power-profiles-daemon` or, increasingly, `tuned`; ASUS
//! installs `asusd` (which also owns RGB/Aura, hence advisory); Bazzite's
//! Ally image ships `hhd` (Handheld Daemon).
//!
//! Surfaced at daemon startup (a loud warning for hard rivals) and live
//! over D-Bus via `get_power_conflicts` (rivals) and `get_advisory_daemons`
//! (advisory), which `hpdctl doctor` / `hpdctl status` render.

#[cfg(not(feature = "simulator"))]
use tracing::warn;

/// Hard rivals detected by a well-known **D-Bus name** — they own the same
/// TDP / `platform_profile` / charge surfaces hpd does and must not co-run.
/// `hpdctl doctor --fix` masks the matching systemd unit.
///
/// Each entry is `(friendly_name, well_known_dbus_name)`.
/// * `power-profiles-daemon` — owns `platform_profile` + EPP (GNOME/KDE).
/// * `steamos-manager` — Valve's TDP / charge / fan backend behind Steam
///   Game Mode's performance panel.
/// * `tuned` — Fedora/Bazzite's increasingly-default power tuner; owns
///   `platform_profile` + EPP. (Its `tuned-ppd` shim *also* claims
///   `net.hadess.PowerProfiles`, so a tuned-ppd host may match both the
///   PPD and the tuned entries — harmless double-report.)
pub const RIVAL_POWER_DAEMONS: &[(&str, &str)] = &[
    ("power-profiles-daemon", "net.hadess.PowerProfiles"),
    ("steamos-manager", "com.steampowered.SteamOSManager1"),
    ("tuned", "com.redhat.tuned"),
];

/// Hard rivals detected by an **active systemd unit** — same "must not
/// co-run, `doctor --fix` masks it" status as [`RIVAL_POWER_DAEMONS`], but
/// they own no well-known bus name so `NameHasOwner` cannot see them.
///
/// Each entry is `(friendly_name, unit_pattern)` where `unit_pattern` is a
/// shell-style glob for `ListUnitsByPatterns` (and the unit `doctor --fix`
/// masks; a templated pattern like `hhd@*.service` masks via its template).
/// * `hhd` — Feral-independent Handheld Daemon (hhd-dev), Bazzite's default
///   on the ROG Ally; a full handheld daemon that owns TDP and the platform
///   profile. Runs as the templated `hhd@<user>.service`.
pub const RIVAL_UNITS: &[(&str, &str)] = &[("hhd", "hhd@*.service")];

/// Advisory daemons detected by a well-known **D-Bus name** — they only
/// touch power-adjacent surfaces and are legitimately wanted, so hpd reports
/// them but `hpdctl doctor --fix` never masks them.
///
/// Each entry is `(friendly_name, well_known_dbus_name)`.
/// * `gamemoded` — Feral GameMode, activated by Steam / Lutris / Heroic
///   around a game to raise the governor to `performance`.
/// * `asusd` — the asus-linux.org daemon. It *does* drive
///   `platform_profile`, the fan curve and the charge limit on ASUS, so it
///   genuinely overlaps hpd — but it also owns keyboard RGB / Aura / panel
///   overdrive, so masking it would break those. Reported loudly, never
///   masked: the user picks which daemon owns power.
pub const ADVISORY_POWER_DAEMONS: &[(&str, &str)] = &[
    ("gamemoded", "com.feralinteractive.GameMode"),
    ("asusd", "org.asuslinux.Daemon"),
];

/// Advisory daemons detected by an **active systemd unit** (no bus name).
///
/// Each entry is `(friendly_name, unit_pattern)`.
/// * `auto-cpufreq` — manages the CPU governor / EPP only, none of hpd's
///   core surfaces, so it is purely informational.
pub const ADVISORY_UNITS: &[(&str, &str)] = &[("auto-cpufreq", "auto-cpufreq.service")];

/// Friendly names of every hard rival ([`RIVAL_POWER_DAEMONS`] +
/// [`RIVAL_UNITS`]) that is live right now — a competing power daemon
/// fighting hpd over TDP / `platform_profile` / charge.
///
/// Best-effort throughout: a transport failure talking to the bus daemon or
/// to systemd is treated as "nothing detected" rather than an error,
/// because this is advisory telemetry, not an authorization decision. An
/// empty vector means `hpd` is the sole power owner.
#[cfg(not(feature = "simulator"))]
pub async fn power_conflicts(conn: &zbus::Connection) -> Vec<String> {
    let mut found = live_owners(conn, RIVAL_POWER_DAEMONS).await;
    found.extend(active_units(conn, RIVAL_UNITS).await);
    found
}

/// Friendly names of every advisory daemon ([`ADVISORY_POWER_DAEMONS`] +
/// [`ADVISORY_UNITS`]) that is live. Same best-effort guarantees as
/// [`power_conflicts`], but these are reported only and never masked. An
/// empty vector means no advisory daemon is live.
#[cfg(not(feature = "simulator"))]
pub async fn advisory_daemons(conn: &zbus::Connection) -> Vec<String> {
    let mut found = live_owners(conn, ADVISORY_POWER_DAEMONS).await;
    found.extend(active_units(conn, ADVISORY_UNITS).await);
    found
}

/// Return the friendly names of the `(name, bus_name)` pairs whose
/// well-known D-Bus name currently has an owner. Uses
/// `org.freedesktop.DBus.NameHasOwner`, which reports current ownership
/// **without** D-Bus-activating the service, so checking never revives a
/// masked daemon. A failure reaching the bus daemon is treated as "nothing
/// detected" — this is advisory telemetry, not an authorization decision.
#[cfg(not(feature = "simulator"))]
async fn live_owners(conn: &zbus::Connection, candidates: &[(&str, &str)]) -> Vec<String> {
    let proxy = match zbus::Proxy::new(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "could not reach the bus daemon to detect competing power daemons");
            return Vec::new();
        }
    };

    let mut found = Vec::new();
    for &(name, bus_name) in candidates {
        let owned: bool = proxy
            .call("NameHasOwner", &(bus_name,))
            .await
            .unwrap_or(false);
        if owned {
            found.push(name.to_string());
        }
    }
    found
}

/// Return the friendly names of the `(name, unit_pattern)` pairs that have
/// at least one **active** (or activating) systemd unit matching the
/// pattern. For daemons that own no well-known D-Bus name (Handheld Daemon,
/// auto-cpufreq), so `NameHasOwner` cannot see them.
///
/// Asks `org.freedesktop.systemd1`'s `ListUnitsByPatterns`, a read-only
/// query (allowed under the daemon's `ProtectSystem=strict` sandbox) that —
/// like `NameHasOwner` — only inspects, never starts, a unit. Best-effort:
/// any failure reaching systemd (e.g. a non-systemd host) yields "nothing
/// detected".
#[cfg(not(feature = "simulator"))]
async fn active_units(conn: &zbus::Connection, candidates: &[(&str, &str)]) -> Vec<String> {
    // One element of systemd's `ListUnitsByPatterns` reply array. We only
    // read the unit name (field 0); the rest is decoded to satisfy the
    // signature `a(ssssssouso)` and discarded.
    type SystemdUnit = (
        String,
        String,
        String,
        String,
        String,
        String,
        zbus::zvariant::OwnedObjectPath,
        u32,
        String,
        zbus::zvariant::OwnedObjectPath,
    );

    let proxy = match zbus::Proxy::new(
        conn,
        "org.freedesktop.systemd1",
        "/org/freedesktop/systemd1",
        "org.freedesktop.systemd1.Manager",
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "could not reach systemd to detect unit-only competing daemons");
            return Vec::new();
        }
    };

    // Restrict to running units so a merely-installed (inactive) unit is not
    // reported as a live conflict.
    let states: &[&str] = &["active", "activating"];
    let mut found = Vec::new();
    for &(name, pattern) in candidates {
        let patterns: &[&str] = &[pattern];
        let units: Vec<SystemdUnit> = proxy
            .call("ListUnitsByPatterns", &(states, patterns))
            .await
            .unwrap_or_default();
        if !units.is_empty() {
            found.push(name.to_string());
        }
    }
    found
}

/// Simulator builds run on the session bus with no real platform power
/// management, so there is nothing to conflict with — report none.
#[cfg(feature = "simulator")]
pub async fn power_conflicts(_conn: &zbus::Connection) -> Vec<String> {
    Vec::new()
}

/// Simulator builds have no GameMode / advisory daemons to detect either.
#[cfg(feature = "simulator")]
pub async fn advisory_daemons(_conn: &zbus::Connection) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bus-name list holds dotted well-known names.
    #[test]
    fn bus_names_are_well_formed() {
        for (name, bus) in RIVAL_POWER_DAEMONS.iter().chain(ADVISORY_POWER_DAEMONS) {
            assert!(!name.is_empty(), "daemon friendly name is empty");
            assert!(
                bus.contains('.') && !bus.starts_with('.') && !bus.ends_with('.'),
                "daemon bus name {bus} is not a dotted well-known name"
            );
        }
    }

    /// Every unit-pattern list holds a friendly name plus a `.service`
    /// pattern (what `ListUnitsByPatterns` matches and `doctor --fix` masks).
    #[test]
    fn unit_patterns_are_well_formed() {
        for (name, pattern) in RIVAL_UNITS.iter().chain(ADVISORY_UNITS) {
            assert!(!name.is_empty(), "daemon friendly name is empty");
            assert!(
                pattern.ends_with(".service"),
                "unit pattern {pattern} is not a .service unit"
            );
        }
    }

    /// Hard rivals (`doctor --fix` masks them) and advisory daemons (only
    /// reported) must stay disjoint across *both* detection axes, or the
    /// repair would mask something it promised to leave alone (e.g. asusd,
    /// which also owns keyboard RGB).
    #[test]
    fn rival_and_advisory_lists_are_disjoint() {
        let rival_ids: Vec<&str> = RIVAL_POWER_DAEMONS
            .iter()
            .chain(RIVAL_UNITS)
            .map(|(_, id)| *id)
            .collect();
        let advisory_ids: Vec<&str> = ADVISORY_POWER_DAEMONS
            .iter()
            .chain(ADVISORY_UNITS)
            .map(|(_, id)| *id)
            .collect();
        for id in &rival_ids {
            assert!(
                !advisory_ids.contains(id),
                "{id} is in both the rival and advisory lists"
            );
        }
    }
}
