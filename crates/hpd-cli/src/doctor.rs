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

/// Systemd units `hpdctl doctor --fix` neutralizes (disable + mask). These
/// are the **hard rivals** the daemon detects in
/// `hpd_dbus::conflicts::{RIVAL_POWER_DAEMONS, RIVAL_UNITS}` — the CLI
/// deliberately does not depend on hpd-dbus, so the list is mirrored here.
/// Advisory daemons (asusd, gamemoded, auto-cpufreq) are intentionally
/// absent: they are reported, never masked.
///
/// Each entry is a full unit name. A templated entry like `hhd@.service`
/// masks the *template* (blocking every instance); its running instances
/// are stopped via the `hhd@*.service` glob in [`neutralize_rivals_as_root`].
const RIVAL_UNITS: &[&str] = &[
    "power-profiles-daemon.service",
    "steamos-manager.service",
    "tuned.service",
    "hhd@.service",
];

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

/// Read-only health report for `hpdctl doctor`. Prints the doctor banner
/// and the shared health block ([`print_health`]).
pub async fn report(proxy: &PowerDaemonProxy<'_>) {
    println!("🩺 hpd doctor — is hpd the sole power manager?");
    println!();
    print_health(proxy).await;
}

/// A compact one-line health summary for `hpdctl monitor`, where the full
/// [`print_health`] block would be too noisy to redraw every second. Reports
/// the single most important fact, in priority order: a competing daemon
/// fighting hpd, then a missing polkit policy, then an advisory daemon
/// (GameMode), else the all-clear. The string already includes the
/// `🩺 Health:` label's value (the caller supplies the label/padding).
pub async fn health_summary(proxy: &PowerDaemonProxy<'_>) -> String {
    // A polkit transport error is treated as "unknown, not broken" here:
    // monitor refreshes once a second and we would rather not flash a red
    // warning on a transient hiccup. Only a definite `false` is a problem.
    let polkit_bad = matches!(proxy.get_diagnostics().await, Ok((false, _)));
    let conflicts = proxy.get_power_conflicts().await.unwrap_or_default();
    let advisory = proxy.get_advisory_daemons().await.unwrap_or_default();

    if !conflicts.is_empty() {
        format!(
            "⚠️  {} fighting hpd — run `hpdctl doctor --fix`",
            conflicts.join(", ")
        )
    } else if polkit_bad {
        "⚠️  polkit not installed — run `hpdctl fix-polkit`".to_string()
    } else if !advisory.is_empty() {
        format!(
            "ℹ️  {} active (governor may move) · hpd otherwise owns power",
            advisory.join(", ")
        )
    } else {
        "✅ all good — hpd is the sole power manager".to_string()
    }
}

/// Render the shared "is hpd the sole power manager?" health block, used by
/// both `hpdctl doctor` and `hpdctl status`. Queries the running daemon and
/// prints four lines — polkit registration, competing power daemons (hard
/// rivals), advisory daemons (GameMode), the gamescope session hint — then a
/// one-line verdict.
///
/// Returns `true` when everything is healthy (nothing for the user to fix).
/// Degrades gracefully against an older daemon: a missing `get_diagnostics`
/// reads as "could not query", and a missing `get_advisory_daemons` simply
/// omits the GameMode line rather than erroring.
pub async fn print_health(proxy: &PowerDaemonProxy<'_>) -> bool {
    let diagnostics = proxy.get_diagnostics().await;
    let conflicts = proxy.get_power_conflicts().await;
    let advisory = proxy.get_advisory_daemons().await;

    let polkit_bad = match &diagnostics {
        Ok((true, _)) => {
            println!("  polkit:             ✅ installed (privileged commands work)");
            false
        }
        Ok((false, missing)) => {
            println!("  polkit:             ❌ NOT installed — privileged commands are denied");
            if !missing.is_empty() {
                println!("                      unregistered: {}", missing.join(", "));
            }
            true
        }
        Err(_) => {
            println!("  polkit:             ❔ could not query the daemon (is hpd running?)");
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

    // Advisory daemons (GameMode, asusd, auto-cpufreq) are never a
    // "problem" — they are legitimately wanted (asusd also drives RGB, etc.)
    // — so they are reported but never flip the verdict or get masked.
    match &advisory {
        Ok(found) if found.is_empty() => {
            println!("  advisory tools:     ✅ none touching power surfaces");
        }
        Ok(found) => {
            println!(
                "  advisory tools:     ℹ️  {} live — may also touch power (informational; not masked)",
                found.join(", ")
            );
        }
        // Older daemon without get_advisory_daemons: omit the line silently.
        Err(_) => {}
    }

    if gamescope_session() {
        println!("  session:            🎮 gamescope (Steam Game Mode) — steamos-manager is the TDP backend here");
    }

    println!("---------------------------------------");
    let healthy = !polkit_bad && !conflicts_bad;
    if healthy {
        println!("✅ All good — hpd is the sole power manager.");
    } else {
        println!("→ Fix it in one step:  hpdctl doctor --fix");
    }
    healthy
}

/// Best-effort detection that we are inside a Steam Game Mode (gamescope)
/// session. Runs client-side because the CLI shares the user's session
/// environment, which a root system daemon does not see — this is why the
/// gamescope hint lives in `hpdctl` and not in `hpd_dbus::conflicts`.
/// Purely informational context; never affects the health verdict.
fn gamescope_session() -> bool {
    std::env::var("GAMESCOPE_WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_CURRENT_DESKTOP")
            .map(|d| d.to_ascii_lowercase().contains("gamescope"))
            .unwrap_or(false)
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
/// `steamos-manager`, or a template-instantiated one like `hhd@deck`, from
/// being revived on demand. All best-effort and idempotent — a rival that
/// is absent or already masked is fine.
fn neutralize_rivals_as_root() {
    for unit in RIVAL_UNITS {
        let _ = Command::new("systemctl")
            .args(["disable", "--now", unit])
            .status();
        // `disable --now` does not stop running instances of a *template*
        // unit (`hhd@.service` has no instance of its own), so stop them
        // explicitly via the instance glob before masking the template.
        if unit.contains('@') {
            let glob = unit.replace("@.service", "@*.service");
            let _ = Command::new("systemctl").args(["stop", &glob]).status();
        }
        let masked = matches!(
            Command::new("systemctl").args(["mask", unit]).status(),
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
