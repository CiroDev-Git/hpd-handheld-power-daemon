#!/bin/bash

set -euo pipefail

echo "🔨 1. Compiling HPD Release..."
cargo build --release

echo "📦 2. Installing binaries in /usr/local/bin..."

sudo systemctl stop hpd.service || true

sudo cp target/release/hpd-daemon /usr/local/bin/
sudo cp target/release/hpdctl /usr/local/bin/

echo "⚙️  3. Installing system configs..."
sudo mkdir -p /etc/systemd/system/
sudo mkdir -p /etc/dbus-1/system.d/

sudo cp package/hpd.service /etc/systemd/system/
sudo cp package/dev.cirodev.hpd.conf /etc/dbus-1/system.d/

echo "🚀 4. Reloading daemons and starting HPD..."
sudo systemctl daemon-reload
sudo systemctl reload dbus
sudo systemctl enable --now hpd.service

echo ""
echo "✅ Installation completed successfully!"
echo "See logs in real time using: journalctl -fu hpd"