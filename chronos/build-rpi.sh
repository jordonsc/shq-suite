#!/bin/bash
# Build Chronos for Raspberry Pi (ARM64) using Cross with Podman.
# Wayland is loaded via dlopen at runtime, so no target sysroot libs are needed.

set -e

export CROSS_CONTAINER_ENGINE=podman

# Clean build script artifacts to avoid GLIBC mismatch from prior host builds
if [ -d "target/release/build" ] || [ -d "target/debug/build" ]; then
    echo "Cleaning build script artifacts..."
    rm -rf target/release/build target/debug/build
fi

TARGET="aarch64-unknown-linux-gnu"

if [ "$1" = "--debug" ] || [ "$1" = "-d" ]; then
    echo "Building debug version for Raspberry Pi (ARM64)..."
    cross build --target $TARGET
    BIN="target/$TARGET/debug/chronos"
else
    echo "Building release version for Raspberry Pi (ARM64)..."
    cross build --release --target $TARGET
    BIN="target/$TARGET/release/chronos"
fi

echo
echo "Preparing build payload..."
rm -rf build
mkdir -p build
cp "$BIN" build/chronos

echo
echo "Build payload ready: build/chronos"
echo
