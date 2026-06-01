// SPDX-License-Identifier: GPL-3.0-or-later

//! Competing power-daemon detection.
//!
//! `hpd` expects to be the sole manager of the platform's power knobs
//! (TDP via the ASUS firmware attributes, the ACPI platform profile, the
//! EC fan curve, the battery charge threshold). Other system power
//! daemons write the *same* sysfs/ACPI surfaces, so running one alongside
//! `hpd` makes the last writer win and the effective state flap. The two
//! seen in the wild on handheld images are `power-profiles-daemon` (owns
//! `platform_profile` + EPP) and Valve's `steamos-manager` (the TDP /
//! charge / fan backend behind Steam Game Mode's performance panel).
//!
//! This module only *detects* — it asks the bus daemon whether each
//! rival's well-known name currently has an owner (i.e. the daemon is
//! running, or D-Bus-activated and live). It never stops anything: the
//! daemon is sandboxed (`ProtectSystem=strict`) and cannot, and silently
//! disabling another package's service is the wrong layer. The repair is
//! the user-side `hpdctl doctor --fix`, mirroring the polkit split where
//! the daemon detects ([`crate::polkit::missing_actions`]) and the CLI
//! repairs ([`hpdctl fix-polkit`]).
//!
//! Surfaced at daemon startup (a loud warning) and live over D-Bus via
//! `get_power_conflicts`, which `hpdctl doctor` / `hpdctl status` render.

#[cfg(not(feature = "simulator"))]
use tracing::warn;

/// Power daemons known to fight `hpd` over the same sysfs/ACPI surfaces.
///
/// Each entry is `(friendly_name, well_known_dbus_name)`. The friendly
/// name doubles as the systemd unit stem `hpdctl doctor --fix` masks
/// (`<friendly_name>.service`); keep the two consistent if a rival is
/// added here.
pub const RIVAL_POWER_DAEMONS: &[(&str, &str)] = &[
    ("power-profiles-daemon", "net.hadess.PowerProfiles"),
    ("steamos-manager", "com.steampowered.SteamOSManager1"),
];

/// Friendly names of [`RIVAL_POWER_DAEMONS`] whose D-Bus name currently
/// has an owner — i.e. a competing power daemon is live right now.
///
/// Uses `org.freedesktop.DBus.NameHasOwner`, which reports current
/// ownership **without** D-Bus-activating the service, so it never
/// revives a masked rival just by checking. Best-effort: a transport
/// failure talking to the bus daemon is treated as "nothing detected"
/// rather than an error, because this is advisory telemetry, not an
/// authorization decision. An empty vector means `hpd` is the sole power
/// owner.
#[cfg(not(feature = "simulator"))]
pub async fn power_conflicts(conn: &zbus::Connection) -> Vec<String> {
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
    for &(name, bus_name) in RIVAL_POWER_DAEMONS {
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

/// Simulator builds run on the session bus with no real platform power
/// management, so there is nothing to conflict with — report none.
#[cfg(feature = "simulator")]
pub async fn power_conflicts(_conn: &zbus::Connection) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rivals_are_well_formed() {
        for (name, bus) in RIVAL_POWER_DAEMONS {
            assert!(!name.is_empty(), "rival friendly name is empty");
            assert!(
                bus.contains('.') && !bus.starts_with('.') && !bus.ends_with('.'),
                "rival bus name {bus} is not a dotted well-known name"
            );
        }
    }
}
