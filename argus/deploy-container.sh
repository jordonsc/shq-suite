#!/usr/bin/env bash
# Build + deploy Argus as a rootless Podman Quadlet systemd --user service on atlas.
#
# Mirrors the atlas qdrant/rag-serve pattern: a localhost/argus:latest image built
# ON atlas from a minimal synced source tree, run by a ~/.config/containers/systemd/
# argus.container Quadlet unit. The image is self-contained (multi-stage build);
# config.yaml, the private seed, the secrets env file, and the case journal are
# bind-mounted, NOT baked in.
#
# PREREQS on atlas (one-off, already true today): Podman, user systemd, and
# `sudo loginctl enable-linger <user>` so the service runs without a login.
#
# This script does NOT touch secrets. Before the first start, the host must have:
#   ~/.config/argus/config.yaml   (private; mirrored from shq-suite-config)
#   ~/.config/argus/seed.md       (private premises seed)
#   ~/.config/argus/argus.env     (from argus.env.example, filled in)
# It will refuse to (re)start if config.yaml or argus.env are missing.
#
# Usage:  argus/deploy-container.sh [user@host]      (default: jordonsc@atlas.shq.sh)
set -euo pipefail

TARGET="${1:-jordonsc@atlas.shq.sh}"
KEY="${ARGUS_SSH_KEY:-$HOME/.ssh/jordon.pem}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_DIR="argus-build"   # remote, under the target user's home

SSH=(ssh -i "$KEY" "$TARGET")
RSYNC_RSH="ssh -i $KEY"

echo "==> Syncing minimal build context to $TARGET:~/$BUILD_DIR"
"${SSH[@]}" "mkdir -p $BUILD_DIR/argus $BUILD_DIR/overwatch/proto"
# Only what the Containerfile needs; target/ and build artefacts excluded. The
# overwatch/proto tree must come along so the argus/proto/voice.proto symlink
# resolves inside the build.
rsync -az --delete -e "$RSYNC_RSH" \
    --exclude='target/' \
    --exclude='build/' \
    "$REPO_ROOT/argus/" "$TARGET:$BUILD_DIR/argus/"
rsync -az --delete -e "$RSYNC_RSH" \
    "$REPO_ROOT/overwatch/proto/" "$TARGET:$BUILD_DIR/overwatch/proto/"

echo "==> Building localhost/argus:latest on atlas"
"${SSH[@]}" "podman build -f $BUILD_DIR/argus/Containerfile -t localhost/argus:latest $BUILD_DIR"

echo "==> Installing the Quadlet unit + ensuring runtime dirs"
"${SSH[@]}" 'mkdir -p ~/.config/containers/systemd ~/.config/argus ~/.config/argus/web ~/.local/share/argus'
rsync -az -e "$RSYNC_RSH" \
    "$REPO_ROOT/argus/argus.container" \
    "$TARGET:.config/containers/systemd/argus.container"

# The HUD web assets are served from a bind-mount (~/.config/argus/web), so the
# frontend can be updated without rebuilding the image. Sync the current web/.
echo "==> Syncing HUD web assets (bind-mount source)"
rsync -az --delete -e "$RSYNC_RSH" \
    "$REPO_ROOT/argus/web/" "$TARGET:.config/argus/web/"

echo "==> Preflight: config + secrets present?"
"${SSH[@]}" '
  miss=0
  for f in ~/.config/argus/config.yaml ~/.config/argus/argus.env; do
    [ -f "$f" ] || { echo "MISSING: $f"; miss=1; }
  done
  [ -f ~/.config/argus/seed.md ] || echo "WARN: ~/.config/argus/seed.md missing (assessment quality floor)"
  exit $miss
'

echo "==> Reloading + restarting the service"
"${SSH[@]}" 'systemctl --user daemon-reload && systemctl --user restart argus.service && sleep 2 && systemctl --user --no-pager status argus.service | head -20'

echo "==> Done. Logs:  ssh $TARGET 'journalctl --user -u argus.service -f'"
