#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

echo "Building release binary..."
cargo build --release

echo "Installing binary to ~/.local/bin/..."
mkdir -p ~/.local/bin
cp target/release/mixxx-midi-clock ~/.local/bin/

echo "Installing systemd user unit..."
mkdir -p ~/.config/systemd/user/
cp systemd/mixxx-midi-clock.service ~/.config/systemd/user/

echo "Reloading systemd..."
systemctl --user daemon-reload

echo "Enabling and starting service..."
systemctl --user enable --now mixxx-midi-clock

echo ""
echo "Installation complete. Monitor with:"
echo "  journalctl --user -u mixxx-midi-clock -f"
