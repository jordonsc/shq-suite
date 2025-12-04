#!/bin/bash
set -e

echo "========================================="
echo "Overwatch Server - WSL2 Development Setup"
echo "========================================="
echo ""

# Check if running on WSL
if ! grep -qEi "(Microsoft|WSL)" /proc/version &> /dev/null ; then
    echo "Warning: This script is designed for WSL2. Continue anyway? (y/n)"
    read -r response
    if [[ ! "$response" =~ ^[Yy]$ ]]; then
        exit 1
    fi
fi

echo "This script will install:"
echo "  - Rust toolchain (if not already installed)"
echo "  - Protocol Buffers compiler (protoc)"
echo "  - ALSA and PulseAudio libraries for WSL audio"
echo "  - ARM64 cross-compilation toolchain for Raspberry Pi builds"
echo ""
read -p "Continue? (y/n) " -n 1 -r
echo ""
if [[ ! $REPLY =~ ^[Yy]$ ]]; then
    exit 1
fi

echo ""
echo "==> Updating package lists..."
sudo apt update

echo ""
echo "==> Installing build essentials..."
sudo apt install -y \
    build-essential \
    pkg-config \
    curl \
    git

echo ""
echo "==> Installing Protocol Buffers compiler..."
sudo apt install -y protobuf-compiler

echo ""
echo "==> Installing ALSA development libraries..."
sudo apt install -y libasound2-dev

echo ""
echo "==> Installing PulseAudio libraries for WSL audio support..."
sudo apt install -y \
    libpulse-dev \
    pulseaudio-utils \
    libasound2-plugins

echo ""
echo "==> Configuring ALSA to use PulseAudio..."
cat > ~/.asoundrc << 'EOF'
pcm.!default {
    type pulse
}

ctl.!default {
    type pulse
}
EOF
echo "Created ~/.asoundrc"

echo ""
echo "==> Installing Podman for containerized cross-compilation..."
sudo apt install -y podman

echo ""
echo "==> Configuring Podman for rootless operation..."
# Enable cgroup v2 delegation for rootless podman
sudo mkdir -p /etc/systemd/system/user@.service.d
echo "[Service]
Delegate=yes" | sudo tee /etc/systemd/system/user@.service.d/delegate.conf > /dev/null

# Note: On WSL, systemd might not be available, so this is optional
if command -v systemctl &> /dev/null; then
    sudo systemctl daemon-reload
fi

echo ""
if ! command -v rustc &> /dev/null; then
    echo "==> Installing Rust toolchain..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    source "$HOME/.cargo/env"
    echo "Rust installed successfully!"
else
    echo "==> Rust is already installed ($(rustc --version))"
fi

echo ""
echo "==> Adding ARM64 Rust target for Raspberry Pi cross-compilation..."
rustup target add aarch64-unknown-linux-gnu

echo ""
echo "==> Installing 'cross' for cross-compilation..."
cargo install cross --git https://github.com/cross-rs/cross

echo ""
echo "==> Configuring Cross to use Podman..."
mkdir -p ~/.cargo
cat >> ~/.cargo/env << 'EOF'

# Configure Cross to use Podman instead of Docker
export CROSS_CONTAINER_ENGINE=podman
EOF

# Also set for current session
export CROSS_CONTAINER_ENGINE=podman

echo ""
echo "==> Verifying PulseAudio connection..."
if pactl info &> /dev/null; then
    echo "✓ PulseAudio is working"
    pactl info | grep "Server Name"
else
    echo "⚠ Warning: PulseAudio is not responding"
    echo "  Audio may not work. Ensure WSLg is enabled (Windows 11 required)"
fi

echo ""
echo "========================================="
echo "Setup Complete!"
echo "========================================="
echo ""
echo "Next steps:"
echo "  1. Restart your shell or run: source ~/.cargo/env"
echo "  2. Build for local development: cargo build --release"
echo "  3. Build for Raspberry Pi: ./build-rpi.sh"
echo ""
echo "Installed tools:"
echo "  - rustc: $(rustc --version 2>/dev/null || echo 'restart shell to use')"
echo "  - cargo: $(cargo --version 2>/dev/null || echo 'restart shell to use')"
echo "  - cross: $(cross --version 2>/dev/null || echo 'restart shell to use')"
echo "  - podman: $(podman --version)"
echo "  - protoc: $(protoc --version)"
echo ""
echo "Container engine configured: Podman (via CROSS_CONTAINER_ENGINE=podman)"
echo ""
echo "For more information, see README.md"
