// SPDX-License-Identifier: GPL-3.0-or-later

//! `hpdctl doctor` — diagnose and repair hpd's power ownership.
//!
//! Two jobs, one command:
//!
//! * `hpdctl doctor` (read-only) asks the running daemon two questions via
//!   D-Bus — is the polkit policy installed (`get_diagnostics`), and is any
//!   competing power daemon live (`get_power_conflicts`) — and prints a
//!   health report.
//! * `hpdctl doctor --fix` makes hpd the sole power manager: it masks the
//!   competing daemons (`power-profiles-daemon`, `steamos-manager`) and
//!   installs the polkit policy, in one elevated step. It is a superset of
//!   `hpdctl fix-polkit`.
//!
//! The repair needs root but no D-Bus, so `--fix` is intercepted in
//! `main` before the bus is touched (it works with hpd stopped). It
//! mirrors [`crate::fix`]'s elevation: an unprivileged run re-execs itself
//! through `pkexec` (falling back to `sudo`) as `doctor --fix --apply`.
//!
//! Why the CLI and not the daemon: `package/hpd.service` sets
//! `ProtectSystem=strict`, so the daemon cannot disable another service or
//! write to `/usr`. Detection lives in the daemon (`hpd_dbus::conflicts`);
//! repair lives here.

use std::process::Command;

use crate::dbus::PowerDaemonProxy;
use crate::fix;

/// Competing power daemons to neutralize. Mirrors
/// `hpd_dbus::conflicts::RIVAL_POWER_DAEMONS` (the CLI deliberately does
/// not depend on hpd-dbus). Each entry is the systemd unit stem; the unit
/// masked is `<stem>.service`.
const RIVAL_UNITS: &[&str] = &["power-profiles-daemon", "steamos-manager"];

/// Entry point for `hpdctl doctor --fix`.
///
/// `apply` is the internal flag set only on the elevated re-exec — users
/// never pass it. Returns a process exit code.
pub fn run_fix(apply: bool) -> i32 {
    if apply || fix::is_root() {
        return apply_fixes_as_root();
    }
    // Mask the per-user `steamos-manager` proxy first, as the invoking
    // user: `systemctl --user` targets the caller's session manager, which
    // a root pkexec child cannot reach cleanly. Then elevate for the
    // system-level work.
    neutralize_user_steamos();
    elevate_and_reexec()
}

/// Read-only health report for `hpdctl doctor`. Queries the running
/// daemon; degrades gracefully against an older daemon or a missing method.
pub async fn report(proxy: &PowerDaemonProxy<'_>) {
    let diagnostics = proxy.get_diagnostics().await;
    let conflicts = proxy.get_power_conflicts().await;

    println!("🩺 hpd doctor — is hpd the sole power manager?");
    println!();

    let polkit_bad = match &diagnostics {
        Ok((true, _)) => {
            println!("  polkit policy:      ✅ installed (privileged commands work)");
            false
        }
        Ok((false, missing)) => {
            println!("  polkit policy:      ❌ NOT installed — privileged commands are denied");
            if !missing.is_empty() {
                println!("                      unregistered: {}", missing.join(", "));
            }
            true
        }
        Err(_) => {
            println!("  polkit policy:      ❔ could not query the daemon (is hpd running?)");
            true
        }
    };

    let conflicts_bad = match &conflicts {
        Ok(found) if found.is_empty() => {
            println!("  competing daemons:  ✅ none — hpd owns the power knobs");
            false
        }
        Ok(found) => {
            println!(
                "  competing daemons:  ⚠️  active: {} (fighting hpd over TDP / profile / charge)",
                found.join(", ")
            );
            true
        }
        Err(_) => {
            println!("  competing daemons:  ❔ this daemon is too old to report (update hpd)");
            false
        }
    };

    println!();
    if polkit_bad || conflicts_bad {
        println!("→ Fix it in one step:  hpdctl doctor --fix");
    } else {
        println!("✅ All good — hpd is the sole power manager and polkit is installed.");
    }
}

/// Mask the competing system daemons and install the polkit policy. Must
/// run as root; called on the elevated re-exec (or under `sudo hpdctl
/// doctor --fix`). `doctor --fix` is a superset of `fix-polkit`, so it
/// reuses the same polkit installer.
fn apply_fixes_as_root() -> i32 {
    println!("🩺 hpd doctor: making hpd the sole power manager…");
    neutralize_rivals_as_root();

    match fix::apply_as_root() {
        Ok(()) => {
            println!("✅ Done — competing daemons neutralized and polkit policy installed.");
            println!("   Verify with:  hpdctl doctor");
            0
        }
        Err(e) => {
            eprintln!(
                "❌ Competing daemons were masked, but installing the polkit policy failed: {e}"
            );
            eprintln!(
                "   (Are you root? This step writes to /usr/share/polkit-1 and reloads polkit.)"
            );
            1
        }
    }
}

/// `systemctl disable --now` + `mask` each rival system unit. Masking
/// (symlink to `/dev/null`) is what stops a D-Bus-activated rival like
/// `steamos-manager` from being revived on demand. All best-effort and
/// idempotent — a rival that is absent or already masked is fine.
fn neutralize_rivals_as_root() {
    for stem in RIVAL_UNITS {
        let unit = format!("{stem}.service");
        let _ = Command::new("systemctl")
            .args(["disable", "--now", &unit])
            .status();
        let masked = matches!(
            Command::new("systemctl").args(["mask", &unit]).status(),
            Ok(status) if status.success()
        );
        if masked {
            println!("  • neutralized {unit}");
        }
    }
}

/// Mask the per-user `steamos-manager` proxy. Run as the invoking user
/// (no elevation), because `systemctl --user` acts on the caller's session
/// manager. Best-effort and idempotent.
fn neutralize_user_steamos() {
    let _ = Command::new("systemctl")
        .args(["--user", "disable", "--now", "steamos-manager.service"])
        .status();
    let _ = Command::new("systemctl")
        .args(["--user", "mask", "steamos-manager.service"])
        .status();
}

/// Re-exec `hpdctl doctor --fix --apply` as root via `pkexec` (graphical
/// prompt, preferred on handheld desktop sessions) or `sudo` (terminal).
/// Returns the elevated process's exit code.
fn elevate_and_reexec() -> i32 {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("❌ Cannot locate the hpdctl executable to elevate: {e}");
            return 1;
        }
    };

    let tool = if fix::in_path("pkexec") {
        "pkexec"
    } else if fix::in_path("sudo") {
        "sudo"
    } else {
        eprintln!(
            "❌ Need root to neutralize competing daemons, but neither pkexec nor sudo was found."
        );
        eprintln!("   Re-run as root:  sudo hpdctl doctor --fix");
        return 1;
    };

    println!("🔐 Requesting administrator access via {tool}…");
    match Command::new(tool)
        .arg(&exe)
        .args(["doctor", "--fix", "--apply"])
        .status()
    {
        Ok(status) if status.success() => 0,
        Ok(status) => {
            eprintln!("❌ The privileged step did not complete (authentication cancelled?).");
            status.code().unwrap_or(1)
        }
        Err(e) => {
            eprintln!("❌ Failed to run {tool}: {e}");
            1
        }
    }
}
