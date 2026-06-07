#!/bin/bash
# Install hpd-daemon and hpdctl system-wide.
# Requires root (uses sudo). Tested on Arch / Fedora / Debian families.
#
# Runs scripts/doctor.sh first to abort cleanly when prerequisites
# are missing (cargo, rustc MSRV, systemd, D-Bus, polkit). Pass
# --skip-doctor to bypass the preflight if you know what you're doing.

set -euo pipefail

SKIP_DOCTOR="no"
for arg in "$@"; do
    case "$arg" in
        --skip-doctor) SKIP_DOCTOR="yes" ;;
        -h|--help)
            cat <<EOF
Usage: $0 [--skip-doctor]

  --skip-doctor   Don't run scripts/doctor.sh as preflight (advanced).

Builds hpd-daemon + hpdctl in release mode and installs them system-wide.
See scripts/doctor.sh for the prerequisite checks.
EOF
            exit 0
            ;;
        *) echo "Unknown flag: $arg" >&2; exit 2 ;;
    esac
done

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DOCTOR="$SCRIPT_DIR/scripts/doctor.sh"

if [[ "$SKIP_DOCTOR" == "no" ]]; then
    if [[ -x "$DOCTOR" ]]; then
        echo "🩺 0. Running preflight (scripts/doctor.sh)..."
        if ! "$DOCTOR"; then
            cat >&2 <<EOF

❌  Preflight failed. Fix the errors reported above and re-run.

    Faster alternative for end users on Arch / CachyOS / EndeavourOS —
    install the prebuilt AUR package (no Rust toolchain required):
        paru -S hpd-handheld-power-daemon-bin
        # or: yay -S hpd-handheld-power-daemon-bin

    Re-run the doctor anytime:  ./scripts/doctor.sh
    Skip the preflight:         ./install.sh --skip-doctor
EOF
            exit 1
        fi
        echo ""
    elif [[ -r "$DOCTOR" ]]; then
        # File present but not executable (rare — git mode lost). Run
        # under bash explicitly so the install still gets the safety net.
        echo "🩺 0. Running preflight (bash scripts/doctor.sh)..."
        if ! bash "$DOCTOR"; then
            echo "❌  Preflight failed; see errors above." >&2
            exit 1
        fi
        echo ""
    else
        echo "⚠️  scripts/doctor.sh not found; proceeding without preflight." >&2
    fi
fi

echo "🔨 1. Compiling HPD Release..."
# Production build uses the default feature set: `vendor-asus` only.
# To opt into additional vendors append e.g. `--features vendor-lenovo,vendor-valve`.
# The `simulator` feature is intentionally off in installed binaries.
cargo build --release

echo "📦 2. Installing binaries in /usr/local/bin..."
sudo systemctl stop hpd.service || true
sudo install -Dm755 target/release/hpd-daemon /usr/local/bin/hpd-daemon
sudo install -Dm755 target/release/hpdctl     /usr/local/bin/hpdctl

echo "⚙️  3. Installing system configs..."
sudo install -d -m 0755 /etc/systemd/system/
sudo install -d -m 0755 /etc/dbus-1/system.d/
# State directory is normally created by systemd's StateDirectory=, but
# pre-creating it lets the daemon also run cleanly outside systemd.
sudo install -d -m 0700 /var/lib/hpd
# Configuration directory: deploy the example as `.example` only — never
# overwrite an existing /etc/hpd/config.toml so operator edits survive
# re-installs. Operators can `cp config.toml.example config.toml` and
# tune from there.
sudo install -d -m 0755 /etc/hpd

sudo install -Dm644 package/hpd.service                       /etc/systemd/system/hpd.service
sudo install -Dm644 package/dev.cirodev.hpd.conf              /etc/dbus-1/system.d/dev.cirodev.hpd.conf
sudo install -Dm644 package/hpd-example.toml                  /etc/hpd/config.toml.example
sudo install -Dm644 package/polkit/dev.cirodev.hpd.policy     /usr/share/polkit-1/actions/dev.cirodev.hpd.policy
sudo install -Dm644 package/polkit/49-hpd.rules               /usr/share/polkit-1/rules.d/49-hpd.rules

# Version sidecar at /usr/share/hpd/VERSION (single line "X.Y.Z").
# Consumed by external clients that need to know the installed daemon
# version without parsing journalctl or owning the systemd-journal
# group. The hpd-decky-plugin reads this file to enforce its
# `hpdDaemonCompat` range. The version is extracted from the workspace
# `Cargo.toml` so the file always reflects the binaries that were just
# built — no separate source of truth to drift.
HPD_VERSION="$(awk -F\" '
    /^\[workspace\.package\]/ { in_ws = 1; next }
    /^\[/                     { in_ws = 0 }
    in_ws && /^version[[:space:]]*=/ { print $2; exit }
' Cargo.toml)"
if [[ -z "${HPD_VERSION:-}" ]]; then
    echo "❌  Could not extract workspace version from Cargo.toml" >&2
    exit 1
fi
printf '%s\n' "${HPD_VERSION}" | sudo install -Dm644 /dev/stdin /usr/share/hpd/VERSION

echo "🚀 4. Reloading daemons and starting HPD..."
sudo systemctl daemon-reload
sudo systemctl try-reload-or-restart dbus.service
sudo systemctl enable --now hpd.service

echo "🔎 5. Verifying polkit registration..."
# Nudge polkit to pick up the freshly-installed policy + rules. polkit
# watches these directories and normally reloads on its own, but an
# explicit reload makes the verification below deterministic right after
# install (and reloads 49-hpd.rules so wheel members get passwordless
# access immediately). All best-effort: never fail the install over it.
sudo systemctl reload polkit.service 2>/dev/null \
    || sudo systemctl try-restart polkit.service 2>/dev/null \
    || true

if command -v pkaction >/dev/null 2>&1; then
    # pkaction with no args lists every registered action id, one per
    # line — exactly what the daemon's startup self-check verifies over
    # D-Bus. Confirm each of ours is present.
    registered="$(pkaction 2>/dev/null || true)"
    missing=()
    for action in set-tdp set-charge set-profile; do
        if ! printf '%s\n' "$registered" | grep -qxF "dev.cirodev.hpd.$action"; then
            missing+=("dev.cirodev.hpd.$action")
        fi
    done
    if [[ ${#missing[@]} -eq 0 ]]; then
        echo "   ✓ polkit knows all hpd actions (privileged commands will work)."
    else
        echo "   ⚠️  polkit did NOT register: ${missing[*]}" >&2
        echo "      Privileged hpdctl commands would be denied with AuthFailed." >&2
        echo "      Check /usr/share/polkit-1/actions/dev.cirodev.hpd.policy is valid XML," >&2
        echo "      then: sudo systemctl restart polkit" >&2
    fi
else
    echo "   ! pkaction not found; skipping polkit verification (is polkit installed?)." >&2
fi

echo ""
echo "✅ Installation completed successfully!"
echo "   • State file:    /var/lib/hpd/state.toml"
echo "   • Config:        /etc/hpd/config.toml (template: config.toml.example)"
echo "   • Reload config: sudo systemctl reload hpd"
echo "   • Live logs:     journalctl -fu hpd"
echo "   • Uninstall:     ./uninstall.sh"
