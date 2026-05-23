#!/bin/bash
# Uninstall hpd-daemon and hpdctl from the system.
# Persisted state in /var/lib/hpd is preserved unless --purge is passed.

set -euo pipefail

PURGE="no"
if [[ "${1:-}" == "--purge" ]]; then
    PURGE="yes"
fi

echo "🛑 1. Stopping and disabling hpd.service..."
sudo systemctl disable --now hpd.service || true

echo "🧹 2. Removing binaries from /usr/local/bin..."
sudo rm -f /usr/local/bin/hpd-daemon
sudo rm -f /usr/local/bin/hpdctl

echo "🗑️  3. Removing system config files..."
sudo rm -f /etc/systemd/system/hpd.service
sudo rm -f /etc/dbus-1/system.d/dev.cirodev.hpd.conf
sudo rm -f /usr/share/polkit-1/actions/dev.cirodev.hpd.policy
# Drop the example template; the operator's own config.toml (if any)
# is preserved unless --purge is passed.
sudo rm -f /etc/hpd/config.toml.example

echo "🔄 4. Reloading systemd and D-Bus..."
sudo systemctl daemon-reload
sudo systemctl try-reload-or-restart dbus.service

if [[ "$PURGE" == "yes" ]]; then
    echo "💣 5. --purge: removing persisted state at /var/lib/hpd and config at /etc/hpd..."
    sudo rm -rf /var/lib/hpd
    sudo rm -rf /etc/hpd
else
    echo "📁 5. Persisted state at /var/lib/hpd and config at /etc/hpd kept (use --purge to remove)."
fi

echo ""
echo "✅ Uninstall completed."
