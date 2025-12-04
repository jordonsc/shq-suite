#!/bin/bash
# Build for Raspberry Pi using Cross with Podman

set -e

# Ensure CROSS_CONTAINER_ENGINE is set
export CROSS_CONTAINER_ENGINE=podman

# Clean build script artifacts to avoid GLIBC mismatch
# Only clean if build scripts exist from previous host builds
if [ -d "target/release/build" ] || [ -d "target/debug/build" ]; then
    echo "Cleaning build script artifacts..."
    rm -rf target/release/build target/debug/build
fi

# Determine build mode and target architecture
# Use 64-bit ARM for Raspberry Pi 5 with 64-bit OS
TARGET="aarch64-unknown-linux-gnu"

if [ "$1" = "--debug" ] || [ "$1" = "-d" ]; then
    echo "Building debug version for Raspberry Pi 5 (ARM64)..."
    cross build --target $TARGET
    echo ""
    echo "Build complete: target/$TARGET/debug/overwatch"
else
    echo "Building release version for Raspberry Pi 5 (ARM64)..."
    cross build --release --target $TARGET
    echo ""
    echo "Build complete: target/$TARGET/release/overwatch"
fi

# Prepare the build payload
echo "Preparing build payload..."
rm -rf build
mkdir -p build/sounds

# Main binary
if [ "$1" = "--debug" ] || [ "$1" = "-d" ]; then
    cp target/$TARGET/debug/overwatch build/overwatch
else
    cp target/$TARGET/release/overwatch build/overwatch
fi

# Configuration file
cp config.yaml build/config.yaml

# Sounds directory
cp -r sounds build

echo
echo "Build payload ready for deployment."
echo
