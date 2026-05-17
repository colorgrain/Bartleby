#!/usr/bin/env bash
# download_sidecars.sh — Downloads Bartleby sidecar binaries (ffmpeg + mediainfo)
# from the project's GitHub Releases into src-tauri/binaries/.
#
# Usage (from project root):
#   bash scripts/download_sidecars.sh
#
# For Windows, use scripts/download_sidecars.ps1 instead.
# After running, rebuild with: cd src-tauri && cargo build

set -euo pipefail

RELEASE_URL="https://github.com/colorgrain/Bartleby/releases/download/binaries-v1"
BINARIES_DIR="${BINARIES_DIR:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/../src-tauri/binaries}"
mkdir -p "$BINARIES_DIR"

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS-$ARCH" in
  Linux-x86_64)  TRIPLE="x86_64-unknown-linux-gnu" ;;
  Darwin-x86_64) TRIPLE="x86_64-apple-darwin" ;;
  Darwin-arm64)  TRIPLE="aarch64-apple-darwin" ;;
  *)
    echo "ERROR: Unsupported platform $OS-$ARCH"
    echo "For Windows, use: scripts\\download_sidecars.ps1"
    exit 1 ;;
esac

echo "Platform: $OS $ARCH → $TRIPLE"
echo "Output:   $BINARIES_DIR"
echo

download() {
  local name="$1"
  local dest="$BINARIES_DIR/$name"
  if [[ -f "$dest" ]]; then
    echo "  (skip) $name already present"
    return
  fi
  echo "  ↓ $name"
  curl -fsSL --progress-bar "$RELEASE_URL/$name" -o "$dest"
  chmod +x "$dest"
}

# On macOS, download both arch variants — tauri-action builds a universal binary
# and validates resources for both triples even on an arm64 runner.
case "$OS" in
  Darwin)
    download "bartleby-ffmpeg-x86_64-apple-darwin"
    download "bartleby-ffmpeg-aarch64-apple-darwin"
    download "bartleby-mediainfo-x86_64-apple-darwin"
    download "bartleby-mediainfo-aarch64-apple-darwin"
    ;;
  *)
    download "bartleby-ffmpeg-${TRIPLE}"
    download "bartleby-mediainfo-${TRIPLE}"
    ;;
esac

echo
echo "Done. Next step: cd src-tauri && cargo build"
