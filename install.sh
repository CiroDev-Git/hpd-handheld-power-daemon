#!/bin/bash
# Install hpd-daemon and hpdctl system-wide.
# Requires root (uses sudo). Tested on Arch / Fedora / Debian families.

set -euo pipefail

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

echo ""
echo "✅ Installation completed successfully!"
echo "   • State file:    /var/lib/hpd/state.toml"
echo "   • Config:        /etc/hpd/config.toml (template: config.toml.example)"
echo "   • Reload config: sudo systemctl reload hpd"
echo "   • Live logs:     journalctl -fu hpd"
echo "   • Uninstall:     ./uninstall.sh"
