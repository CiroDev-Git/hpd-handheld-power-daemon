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

/// Systemd units `hpdctl doctor --fix` **unconditionally** neutralizes
/// (disable + mask). These are the **hard rivals** the daemon detects in
/// `hpd_dbus::conflicts::{RIVAL_POWER_DAEMONS, RIVAL_UNITS}` — the CLI
/// deliberately does not depend on hpd-dbus, so the list is mirrored here.
/// Advisory daemons (asusd, gamemoded, auto-cpufreq) are intentionally
/// absent: they are reported, never masked.
///
/// `tuned-ppd.service` sits next to `tuned.service`: tuned's
/// PPD-compatibility shim claims `net.hadess.PowerProfiles` on its *own*
/// systemd unit, so masking `tuned.service` alone leaves the shim running
/// and still fighting hpd over the platform profile. `tlp.service` is TLP,
/// a standalone power daemon popular on Arch/CachyOS that writes the same
/// charge/profile/governor surfaces on every AC edge.
///
/// `hhd@.service` (Handheld Daemon) is deliberately **not** in this list —
/// unlike every other entry here, unmasking it back is not just "reinstall
/// the package": on the Xbox Ally X it also owns gamepad remapping, so an
/// unconditional mask has real user-facing fallout if nothing else covers
/// input. See [`neutralize_hhd_as_root`] for the conditional handling.
const RIVAL_UNITS: &[&str] = &[
    "power-profiles-daemon.service",
    "steamos-manager.service",
    "tuned.service",
    "tuned-ppd.service",
    "tlp.service",
];

/// The Handheld Daemon's TDP/platform-profile-owning unit. Bazzite's Ally
/// default; runs templated as `hhd@<user>.service`. Masked only when
/// [`neutralize_hhd_as_root`] confirms something else is covering gamepad
/// input — see that function's doc comment.
const HHD_UNIT: &str = "hhd@.service";

/// InputPlumber (`org.shadowblip.InputPlumber`), the input-router/remapper
/// daemon that is part of the SteamOS/CachyOS Handheld stack. When active,
/// it is the thing actually driving gamepad remapping on the Xbox Ally X,
/// making it safe to neutralize hhd for TDP ownership without losing
/// controller buttons/gyro/rumble.
const INPUTPLUMBER_UNIT: &str = "inputplumber.service";

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
    let ppd_shim_active = proxy.get_ppd_shim_active().await;

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

    // Informational only — never flips `healthy`. An inactive shim caused
    // by a real, unmasked PPD/tuned-ppd is already surfaced above via the
    // "competing daemons" line; this just reports the shim's own state.
    match ppd_shim_active {
        Ok(true) => {
            println!("  compat PPD:         ✅ active (hpd answers for power-profiles-daemon)");
        }
        Ok(false) => {
            println!(
                "  compat PPD:         ➖ inactive (a real PPD/tuned-ppd is not masked — see \
                 'competing daemons' above)"
            );
        }
        // Older daemon without get_ppd_shim_active: omit the line silently.
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

/// `systemctl disable --now` + `mask` each unconditional rival in
/// [`RIVAL_UNITS`], then handle `hhd@.service` separately via
/// [`neutralize_hhd_as_root`]. All best-effort and idempotent — a rival
/// that is absent or already masked is fine.
fn neutralize_rivals_as_root() {
    for unit in RIVAL_UNITS {
        if mask_unit_as_root(unit) {
            println!("  • neutralized {unit}");
        }
    }
    neutralize_hhd_as_root();
}

/// `systemctl disable --now` + `mask` a single system unit. Masking
/// (symlink to `/dev/null`) is what stops a D-Bus-activated rival like
/// `steamos-manager`, or a template-instantiated one like `hhd@deck`, from
/// being revived on demand. Returns whether the mask step reported success.
fn mask_unit_as_root(unit: &str) -> bool {
    let _ = Command::new("systemctl")
        .args(["disable", "--now", unit])
        .status();
    // `disable --now` does not stop running instances of a *template* unit
    // (`hhd@.service` has no instance of its own), so stop them explicitly
    // via the instance glob before masking the template.
    if unit.contains('@') {
        let glob = unit.replace("@.service", "@*.service");
        let _ = Command::new("systemctl").args(["stop", &glob]).status();
    }
    matches!(
        Command::new("systemctl").args(["mask", unit]).status(),
        Ok(status) if status.success()
    )
}

/// Mask `hhd@.service` **only when InputPlumber is covering gamepad
/// input**. Unlike every other entry in [`RIVAL_UNITS`], hhd is not purely
/// a power daemon on the Xbox Ally X: it also remaps the integrated
/// controller (Xbox/ROG buttons, gyro, rumble). Masking it unconditionally
/// — as every earlier version of this command did — wins hpd sole TDP
/// ownership but can silently take those controls away if nothing else in
/// the input stack replaces them.
///
/// `inputplumber.service` active means something else already owns input,
/// so the mask is safe and proceeds exactly like any other rival. Inactive
/// means masking would be a real regression, so we skip it and print both
/// ways forward: install/enable InputPlumber and re-run `doctor --fix`, or
/// keep hhd for input and disable only its TDP "adjustor" in
/// `hhd.settings` (`tdp.enabled = false`) — leaving hpd and hhd to split
/// power and input cleanly instead of hpd taking both.
fn neutralize_hhd_as_root() {
    let input_covered = matches!(
        Command::new("systemctl")
            .args(["is-active", "--quiet", INPUTPLUMBER_UNIT])
            .status(),
        Ok(status) if status.success()
    );

    if input_covered {
        if mask_unit_as_root(HHD_UNIT) {
            println!("  • neutralized {HHD_UNIT} (InputPlumber covers gamepad input)");
        }
        return;
    }

    println!("  • skipped {HHD_UNIT}: it also handles gamepad input on this device, and");
    println!("    {INPUTPLUMBER_UNIT} isn't active to take over. Pick one:");
    println!("      1. Install/enable InputPlumber, then re-run `hpdctl doctor --fix`");
    println!("         (hhd will be masked once it's no longer the only thing owning input).");
    println!("      2. Keep hhd for input and disable only its TDP control: set");
    println!("         `tdp.enabled = false` in hhd's settings instead of masking the unit.");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Anti-drift guard for Audit §4.2 (2026-07). `hpdctl` deliberately
    /// does not depend on `hpd-dbus` at *runtime* (see the doc comment on
    /// [`RIVAL_UNITS`]), so this mask list is a hand-mirrored copy of what
    /// `hpd_dbus::conflicts` actually detects — nothing else enforces that
    /// the mirror stays in sync. Without this test, a rival added only to
    /// the detection side would show up in `hpdctl doctor`'s report but
    /// `doctor --fix` would silently never mask it. The `hpd-dbus`
    /// dependency here is dev-only (see `Cargo.toml`), so it adds no
    /// coupling to the shipped `hpdctl` binary.
    #[test]
    fn every_detected_rival_has_a_mask_target() {
        use hpd_dbus::conflicts::{RIVAL_POWER_DAEMONS, RIVAL_UNITS as DETECTED_RIVAL_UNITS};

        for (friendly_name, _) in RIVAL_POWER_DAEMONS
            .iter()
            .chain(DETECTED_RIVAL_UNITS.iter())
        {
            // hhd is masked conditionally (only when InputPlumber covers
            // input — see `neutralize_hhd_as_root`), not via the flat
            // RIVAL_UNITS list, so it is exempt from the naive
            // "<friendly_name>.service" mapping this test otherwise checks.
            if *friendly_name == "hhd" {
                continue;
            }
            let expected_unit = format!("{friendly_name}.service");
            assert!(
                RIVAL_UNITS.contains(&expected_unit.as_str()),
                "hpd_dbus::conflicts detects '{friendly_name}' as a hard rival, but hpd-cli's \
                 doctor::RIVAL_UNITS has no mask target for it (expected '{expected_unit}'). \
                 Update RIVAL_UNITS in hpd-cli/src/doctor.rs to keep `doctor --fix` in sync with \
                 what hpd_dbus::conflicts reports.",
            );
        }
    }

    /// Complement of the above: every non-hhd entry in this crate's own
    /// mask list must actually correspond to something the daemon detects
    /// — otherwise `doctor --fix` would mask a unit `hpdctl doctor`'s
    /// report never explains, confusing a user who checks why a service
    /// disappeared. `tuned-ppd.service` is the one deliberate exception:
    /// it shares detection with `tuned` (`hpd_dbus::conflicts`' module
    /// docs note the shim claims the same D-Bus name), so it has no
    /// separate friendly-name entry of its own.
    #[test]
    fn every_mask_target_traces_back_to_a_detected_rival() {
        use hpd_dbus::conflicts::{RIVAL_POWER_DAEMONS, RIVAL_UNITS as DETECTED_RIVAL_UNITS};

        let detected_units: Vec<String> = RIVAL_POWER_DAEMONS
            .iter()
            .chain(DETECTED_RIVAL_UNITS.iter())
            .map(|(name, _)| format!("{name}.service"))
            .collect();

        for unit in RIVAL_UNITS {
            if *unit == "tuned-ppd.service" {
                continue;
            }
            assert!(
                detected_units.iter().any(|d| d == unit),
                "hpd-cli's doctor::RIVAL_UNITS masks '{unit}', but no entry in \
                 hpd_dbus::conflicts detects it under a matching friendly name — either add the \
                 detection side, or document why this unit is a deliberate exception (like \
                 tuned-ppd.service) in this test.",
            );
        }
    }
}
