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

sudo install -Dm644 package/hpd.service              /etc/systemd/system/hpd.service
sudo install -Dm644 package/dev.cirodev.hpd.conf     /etc/dbus-1/system.d/dev.cirodev.hpd.conf

echo "🚀 4. Reloading daemons and starting HPD..."
sudo systemctl daemon-reload
sudo systemctl try-reload-or-restart dbus.service
sudo systemctl enable --now hpd.service

echo ""
echo "✅ Installation completed successfully!"
echo "   • State file:    /var/lib/hpd/state.toml"
echo "   • Live logs:     journalctl -fu hpd"
echo "   • Uninstall:     ./uninstall.sh"
