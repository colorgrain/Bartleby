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

# ── Where the binaries come from ──────────────────────────────────────────────
# RELEASE_URL points to a GitHub Release used ONLY as storage for the sidecars.
# It is NOT a Bartleby release: app versions are the v0.1.0-N tags.
# The tag name states exactly what it contains:
#   sidecars-ffmpeg-<ffmpeg version>-mediainfo-<mediainfo version>
# To update the sidecars: publish a NEW release named the same way with the 8
# bartleby-* files (+ SHA256SUMS), change RELEASE_URL here AND in
# download_sidecars.ps1, replace scripts/sidecars.sha256 with the new
# SHA256SUMS, and re-run (outdated local binaries are replaced automatically).
# Keep the old release: older Bartleby commits still download from it.
# Sources used for 9.0.2 / 26.10: see CLAUDE.md, section "Sidecars".
RELEASE_URL="https://github.com/colorgrain/Bartleby/releases/download/sidecars-ffmpeg-9.0.2-mediainfo-26.10"
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

# Expected SHA-256 of every sidecar, versioned in the repository: a replaced
# or corrupted release asset, or a truncated earlier download, is rejected.
SUMS_FILE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/sidecars.sha256"

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

download() {
  local name="$1"
  local dest="$BINARIES_DIR/$name"
  local expected
  expected="$(awk -v n="$name" '$2 == n { print $1 }' "$SUMS_FILE")"
  if [[ -z "$expected" ]]; then
    echo "ERROR: no checksum for $name in $SUMS_FILE"; exit 1
  fi
  if [[ -f "$dest" ]]; then
    if [[ "$(sha256_of "$dest")" == "$expected" ]]; then
      echo "  (ok)   $name already present"
      return
    fi
    echo "  (stale) $name differs from the expected version — downloading again"
  fi
  echo "  ↓ $name"
  # Download to a temporary name, check, then move: an interrupted download
  # never leaves a truncated binary under the final name.
  curl -fsSL --progress-bar "$RELEASE_URL/$name" -o "$dest.part"
  local got
  got="$(sha256_of "$dest.part")"
  if [[ "$got" != "$expected" ]]; then
    rm -f "$dest.part"
    echo "ERROR: SHA-256 mismatch for $name"
    echo "  expected $expected"
    echo "  got      $got"
    exit 1
  fi
  chmod +x "$dest.part"
  mv -f "$dest.part" "$dest"
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
