// SPDX-License-Identifier: GPL-3.0-or-later

//! `hpdctl fix-polkit` — install the daemon's polkit policy + rules.
//!
//! This is the one-command recovery for the most common deployment
//! failure: the binary was deployed without its polkit policy (a
//! hand-copied `hpd-daemon`/`hpdctl`, an aborted install), so polkit does
//! not know the `dev.cirodev.hpd.*` actions and every privileged command
//! fails with an opaque `AuthFailed`. See `crate::dbus`'s `get_diagnostics`
//! and the daemon's startup self-check for the detection side.
//!
//! The canonical policy files are embedded into `hpdctl` at build time
//! (`include_str!`), so the fix works without the source tree present on
//! the target. Installing them is a privileged write to `/usr`, so an
//! unprivileged invocation re-execs itself through `pkexec` (falling back
//! to `sudo`) — both rely on polkit's *core* `org.freedesktop.policykit.exec`
//! action, which is always registered even when ours are not.

use std::io;
use std::path::Path;
use std::process::Command;

/// Canonical polkit files, embedded from the repo at build time so the
/// installed `hpdctl` carries the exact policy its daemon expects — no
/// dependency on the source tree being present on the target host.
const POLICY: &str = include_str!("../../../package/polkit/dev.cirodev.hpd.policy");
const RULES: &str = include_str!("../../../package/polkit/49-hpd.rules");

/// System paths polkit reads. `install.sh` and the AUR packages write the
/// same two files; this keeps them byte-identical.
const POLICY_DEST: &str = "/usr/share/polkit-1/actions/dev.cirodev.hpd.policy";
const RULES_DEST: &str = "/usr/share/polkit-1/rules.d/49-hpd.rules";

/// Entry point for `hpdctl fix-polkit`.
///
/// `apply` is the internal flag set only on the elevated re-exec — users
/// never pass it. When set (or when the writes are otherwise attempted as
/// root) it performs the install directly; otherwise it re-execs itself
/// elevated. Returns a process exit code.
pub fn run(apply: bool) -> i32 {
    // Already root (e.g. invoked from a Decky plugin's root backend, or
    // `sudo hpdctl fix-polkit`)? Write directly instead of spawning
    // pkexec/sudo, which may be absent in that environment.
    if apply || is_root() {
        return match apply_as_root() {
            Ok(()) => {
                println!(
                    "✅ polkit policy installed. Privileged commands (TDP / charge / cooling) will work now."
                );
                0
            }
            Err(e) => {
                eprintln!("❌ Could not install the polkit policy: {e}");
                eprintln!(
                    "   (Are you root? This step writes to /usr/share/polkit-1 and reloads polkit.)"
                );
                1
            }
        };
    }
    elevate_and_reexec()
}

/// Re-exec `hpdctl fix-polkit --apply` as root via `pkexec` (preferred —
/// shows a graphical polkit prompt on handheld desktop sessions) or
/// `sudo` (terminal prompt). Returns the elevated process's exit code.
fn elevate_and_reexec() -> i32 {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("❌ Cannot locate the hpdctl executable to elevate: {e}");
            return 1;
        }
    };

    let tool = if in_path("pkexec") {
        "pkexec"
    } else if in_path("sudo") {
        "sudo"
    } else {
        eprintln!(
            "❌ Need root to install the polkit policy, but neither pkexec nor sudo was found."
        );
        eprintln!("   Re-run as root:  sudo hpdctl fix-polkit");
        return 1;
    };

    println!("🔐 Requesting administrator access via {tool}…");
    match Command::new(tool)
        .arg(&exe)
        .args(["fix-polkit", "--apply"])
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

/// Write both polkit files (mode 0644) and reload polkit. Must run as
/// root; the `fs::write` calls fail with `PermissionDenied` otherwise.
///
/// `pub(crate)` so `hpdctl doctor --fix` can reuse it: `doctor` is a
/// superset of `fix-polkit` (it also masks competing daemons), and both
/// should install byte-identical policy.
pub(crate) fn apply_as_root() -> io::Result<()> {
    write_file(POLICY_DEST, POLICY)?;
    write_file(RULES_DEST, RULES)?;
    reload_polkit();
    Ok(())
}

fn write_file(dest: &str, content: &str) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let path = Path::new(dest);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))?;
    println!("  • wrote {dest}");
    Ok(())
}

/// Nudge polkit to pick up the freshly-written files. polkit watches these
/// directories and normally reloads on its own, but an explicit reload
/// makes the fix take effect immediately (and loads 49-hpd.rules so wheel
/// members get passwordless access). All best-effort.
fn reload_polkit() {
    let reloaded = matches!(
        Command::new("systemctl")
            .args(["reload", "polkit.service"])
            .status(),
        Ok(status) if status.success()
    );
    if !reloaded {
        let _ = Command::new("systemctl")
            .args(["try-restart", "polkit.service"])
            .status();
    }
}

/// Whether `cmd` is found on `PATH`. Avoids executing the tool just to
/// probe for it (so we don't spawn `pkexec --version` and friends).
///
/// `pub(crate)` so `hpdctl doctor` reuses the same pkexec/sudo probe.
pub(crate) fn in_path(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(cmd).is_file()))
        .unwrap_or(false)
}

/// Whether we are running with effective UID 0.
///
/// Read from `/proc/self/status` rather than `geteuid(2)` because
/// `unsafe` (and thus a raw libc call) is forbidden workspace-wide. The
/// `Uid:` line is `real  effective  saved  fs`; we want the effective
/// one. Linux-only — on other hosts the read fails and we report
/// non-root, falling back to pkexec/sudo elevation (fine for dev).
///
/// `pub(crate)` so `hpdctl doctor` shares the same root detection.
pub(crate) fn is_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Uid:"))
                .and_then(|rest| rest.split_whitespace().nth(1).map(str::to_owned))
        })
        .map(|euid| euid == "0")
        .unwrap_or(false)
}
