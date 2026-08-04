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
//! desktops ship `power-profiles-daemon` or, increasingly, `tuned`, plus
//! `upower` universally; ASUS installs `asusd` (which also owns RGB/Aura,
//! hence advisory); Bazzite's Ally image ships `hhd` (Handheld Daemon);
//! Arch/CachyOS users commonly reach for TLP as a standalone power
//! manager; and the Decky TDP plugins drive `powerstation` underneath.
//!
//! Observed on a clean CachyOS Handheld image (ROG Xbox Ally X, 2026-08):
//! `power-profiles-daemon` and `steamos-manager` both run out of the box
//! (the latter already having written the PPT rails and left
//! `platform_profile` at `custom`), and `upower` is always live — so a
//! fresh install is *never* conflict-free before `hpdctl doctor --fix`.
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
/// * `powerstation` — ShadowBlip's TDP/GPU service. It is the backend the
///   Decky plugins SimpleDeckyTDP and PowerControl drive, so it writes the
///   same PPT rails hpd owns. Those plugins live inside the plugin loader
///   and are themselves undetectable (no unit, no bus name), but
///   `powerstation` does own both — making it the one handle hpd has on
///   that whole family of TDP tooling.
pub const RIVAL_POWER_DAEMONS: &[(&str, &str)] = &[
    ("power-profiles-daemon", "net.hadess.PowerProfiles"),
    ("steamos-manager", "com.steampowered.SteamOSManager1"),
    ("tuned", "com.redhat.tuned"),
    ("powerstation", "org.shadowblip.PowerStation"),
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
/// * `tlp` — TLP, a popular standalone power-management daemon on
///   Arch/CachyOS. Writes `charge_control_end_threshold`, `platform_profile`
///   / EPP and the CPU governor on every AC/battery edge — the same
///   surfaces and the same "react to the plug event" shape as hpd's own
///   AC-lock, so the two fight on every plug/unplug. No well-known bus
///   name, hence the unit-pattern check.
pub const RIVAL_UNITS: &[(&str, &str)] = &[("hhd", "hhd@*.service"), ("tlp", "tlp.service")];

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
///   (On the ROG Ally family the joystick RGB is driven by the kernel's
///   own `asus_rog_ally` HID driver as a plain `led_class_multicolor`
///   device, so `asusd` buys that hardware nothing — but the advisory
///   classification is global, and on ROG *laptops* Aura really does
///   depend on it.)
/// * `upower` — the desktop's battery-information service. Almost all of
///   it is read-only, but since 1.90 it also *writes*
///   `charge_control_end_threshold` via `EnableChargeThreshold()` — the
///   same file hpd's `ChargeControl` owns, and what the battery-limit
///   toggle in KDE/GNOME power settings actually drives. Unlike a real
///   rival it never acts on its own (no AC-edge or boot reassertion), so
///   the two only diverge when a user sets the limit from the desktop UI
///   instead of `hpdctl charge`; hpd re-asserts its own value on the next
///   boot/resume regardless. Never maskable: the whole desktop battery
///   stack — including the critical-battery shutdown action — depends on
///   it, so masking it would be far more destructive than the divergence
///   it prevents.
pub const ADVISORY_POWER_DAEMONS: &[(&str, &str)] = &[
    ("gamemoded", "com.feralinteractive.GameMode"),
    ("asusd", "org.asuslinux.Daemon"),
    ("upower", "org.freedesktop.UPower"),
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
/// well-known D-Bus name currently has an owner **other than this
/// connection itself**. Uses `org.freedesktop.DBus.GetNameOwner` (rather
/// than the simpler `NameHasOwner`) specifically so that hpd's own
/// `net.hadess.PowerProfiles` compat shim (see `crate::ppd_shim`) —
/// which makes hpd itself the owner of that name — is never
/// misidentified as the real `power-profiles-daemon` rival it exists to
/// stand in for. Neither call D-Bus-activates the service, so checking
/// never revives a masked daemon. A failure reaching the bus daemon is
/// treated as "nothing detected" — this is advisory telemetry, not an
/// authorization decision.
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

    let our_name = conn.unique_name().map(|n| n.as_str());
    let mut found = Vec::new();
    for &(name, bus_name) in candidates {
        // `GetNameOwner` errors (`NameHasNoOwner`) when nobody owns it —
        // that's "not found", not a failure worth logging.
        let owner: Option<String> = proxy.call("GetNameOwner", &(bus_name,)).await.ok();
        if let Some(owner) = owner {
            if Some(owner.as_str()) != our_name {
                found.push(name.to_string());
            }
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

    /// Regression for the PPD-shim false positive: if *we* own a
    /// candidate's well-known name (exactly what happens once
    /// `crate::ppd_shim` claims `net.hadess.PowerProfiles`), it must not
    /// be reported as a rival. Skips (rather than fails) if this sandbox
    /// has no reachable session bus — the assertion that matters is
    /// "never a false positive," which a skipped run doesn't violate.
    #[cfg(not(feature = "simulator"))]
    #[tokio::test]
    async fn live_owners_excludes_names_we_own_ourselves() {
        let Ok(conn) = zbus::Connection::session().await else {
            return;
        };
        let fake_name = "dev.cirodev.hpd.tests.LiveOwnersFakeRival";
        if conn.request_name(fake_name).await.is_err() {
            return; // no usable bus in this sandbox either; skip
        }
        let candidates: &[(&str, &str)] = &[("fake-rival", fake_name)];
        let found = live_owners(&conn, candidates).await;
        assert!(
            found.is_empty(),
            "a name owned by our own connection must never be reported as a rival"
        );
    }

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

    /// Daemons whose advisory classification is load-bearing: masking one
    /// breaks something far more important than the overlap it would fix.
    /// The disjointness test below only catches a daemon *added* to both
    /// lists — it would not catch one **moved** from advisory to rival,
    /// which is exactly the mistake that hurts here, hence this explicit
    /// pin.
    ///
    /// * `upower` — the desktop battery stack, including the
    ///   critical-battery shutdown action. Masking it can cost the user
    ///   unsaved work when the pack runs flat.
    /// * `asusd` — keyboard RGB / Aura on ROG laptops.
    #[test]
    fn safety_critical_daemons_stay_advisory() {
        for id in ["org.freedesktop.UPower", "org.asuslinux.Daemon"] {
            assert!(
                ADVISORY_POWER_DAEMONS.iter().any(|(_, bus)| *bus == id),
                "{id} must stay in ADVISORY_POWER_DAEMONS — see this test's doc comment for why \
                 masking it is not an acceptable trade",
            );
            assert!(
                !RIVAL_POWER_DAEMONS.iter().any(|(_, bus)| *bus == id),
                "{id} was promoted to a hard rival, which makes `doctor --fix` mask it — see this \
                 test's doc comment for why that is not an acceptable trade",
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
