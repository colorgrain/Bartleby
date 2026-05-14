#!/usr/bin/env bash
# download_sidecars.sh — Downloads MediaInfo CLI and FFmpeg for the current platform
# and places them in src-tauri/binaries/ with the Tauri sidecar naming convention.
#
# Usage:
#   bash scripts/download_sidecars.sh
#
# Run on each platform (Linux, macOS) to get the matching binary.
# For Windows, use scripts/download_sidecars.ps1 instead.
#
# After running, rebuild with:
#   cd src-tauri && cargo build        # or npm run build

set -euo pipefail

BINARIES_DIR="$(dirname "$0")/../src-tauri/binaries"
mkdir -p "$BINARIES_DIR"

# ── Detect platform ───────────────────────────────────────────────────────────

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS-$ARCH" in
  Linux-x86_64)   TRIPLE="x86_64-unknown-linux-gnu" ;;
  Linux-aarch64)  TRIPLE="aarch64-unknown-linux-gnu" ;;
  Darwin-arm64)   TRIPLE="aarch64-apple-darwin" ;;
  Darwin-x86_64)  TRIPLE="x86_64-apple-darwin" ;;
  *)
    echo "ERROR: Unsupported platform $OS-$ARCH"
    echo "For Windows: run scripts\\download_sidecars.ps1 instead."
    exit 1
    ;;
esac

echo "Platform: $OS $ARCH → triple: $TRIPLE"
echo "Output dir: $BINARIES_DIR"
echo

# ── MediaInfo CLI ─────────────────────────────────────────────────────────────
# Source officielle : https://mediaarea.net/en/MediaInfo/Download
#
# macOS  → DMG officiel sur la page de téléchargement. ✓
# Windows→ ZIP officiel sur la page de téléchargement. ✓ (voir download_sidecars.ps1)
# Linux  → Situation particulière :
#   • Les packages .deb/.rpm officiels (Ubuntu 24.04 etc.) dépendent de libzen.so et
#     libmediainfo.so — des librairies MediaArea absentes sur les systèmes sans MediaInfo.
#     Inutilisables comme sidecar standalone.
#   • Les AppImages officielles s'arrêtent à la v20.09 (2020). Trop anciennes.
#   • Les "Lambda builds" (mediaarea.net/download/binary/mediainfo/26.05/
#     MediaInfo_CLI_26.05_Lambda_x86_64.zip) sont hébergées par MediaArea et ne
#     dépendent que de libc/libstdc++ (universels). Elles ne figurent pas sur la
#     page de téléchargement principale mais sont produites par MediaArea eux-mêmes
#     pour leurs pipelines AWS Lambda. C'est la seule option portable pour Linux.

MEDIAINFO_VERSION="26.05"  # Update this when a newer version is released

download_mediainfo() {
  local triple="$1"
  local dest="$BINARIES_DIR/mediainfo-${triple}"

  if [[ -f "$dest" ]]; then
    echo "mediainfo-${triple} already present, skipping."
    return
  fi

  local tmpdir; tmpdir="$(mktemp -d)"
  trap "rm -rf $tmpdir" RETURN

  case "$triple" in
    x86_64-unknown-linux-gnu)
      # Lambda build = static-ish binary with only libc/libstdc++ deps; runs on any modern Linux
      local url="https://mediaarea.net/download/binary/mediainfo/${MEDIAINFO_VERSION}/MediaInfo_CLI_${MEDIAINFO_VERSION}_Lambda_x86_64.zip"
      echo "Downloading MediaInfo $MEDIAINFO_VERSION for Linux x86_64..."
      curl -fsSL "$url" -o "$tmpdir/mi.zip"
      unzip -q "$tmpdir/mi.zip" -d "$tmpdir"
      cp "$tmpdir/bin/mediainfo" "$dest"
      ;;
    aarch64-unknown-linux-gnu)
      local url="https://mediaarea.net/download/binary/mediainfo/${MEDIAINFO_VERSION}/MediaInfo_CLI_${MEDIAINFO_VERSION}_Lambda_arm64.zip"
      echo "Downloading MediaInfo $MEDIAINFO_VERSION for Linux ARM64..."
      curl -fsSL "$url" -o "$tmpdir/mi.zip"
      unzip -q "$tmpdir/mi.zip" -d "$tmpdir"
      cp "$tmpdir/bin/mediainfo" "$dest"
      ;;
    x86_64-apple-darwin|aarch64-apple-darwin)
      # macOS DMG: extract binary with 7z (brew install p7zip) or hdiutil (macOS only)
      local url="https://mediaarea.net/download/binary/mediainfo/${MEDIAINFO_VERSION}/MediaInfo_CLI_${MEDIAINFO_VERSION}_Mac.dmg"
      echo "Downloading MediaInfo $MEDIAINFO_VERSION for macOS..."
      curl -fsSL "$url" -o "$tmpdir/mi.dmg"
      if command -v hdiutil &>/dev/null; then
        hdiutil attach -quiet -mountpoint "$tmpdir/mi_vol" "$tmpdir/mi.dmg"
        cp "$tmpdir/mi_vol/MediaInfo" "$dest"
        hdiutil detach -quiet "$tmpdir/mi_vol"
      else
        echo "ERROR: hdiutil not found. Run this script on macOS to extract the macOS binary."
        return
      fi
      # Ad-hoc sign for macOS (required to run as subprocess without quarantine issues)
      if command -v codesign &>/dev/null; then
        codesign -s - "$dest" && echo "  ✓ Ad-hoc signed $dest"
      fi
      ;;
  esac

  chmod +x "$dest"
  echo "  ✓ $dest"
}

# ── FFmpeg ────────────────────────────────────────────────────────────────────
# Sources recommandées par ffmpeg.org/download.html :
#   Linux  → BtbN (GitHub releases) : github.com/BtbN/FFmpeg-Builds
#   macOS  → evermeet.cx            : evermeet.cx/ffmpeg/   ⚠ Intel x86_64 uniquement
#   Windows→ gyan.dev               : gyan.dev/ffmpeg/builds/ (voir download_sidecars.ps1)
#
# Note macOS arm64 : ffmpeg.org ne recommande aucune source officielle pour arm64 macOS.
# evermeet.cx (x86_64) fonctionne sur Apple Silicon via Rosetta 2.

download_ffmpeg() {
  local triple="$1"
  local dest="$BINARIES_DIR/ffmpeg-${triple}"

  if [[ -f "$dest" ]]; then
    echo "ffmpeg-${triple} already present, skipping."
    return
  fi

  local tmpdir; tmpdir="$(mktemp -d)"
  trap "rm -rf $tmpdir" RETURN

  case "$triple" in
    x86_64-unknown-linux-gnu)
      # BtbN GPL static build — recommandé officiellement par ffmpeg.org
      # Archive structure: ffmpeg-master-latest-linux64-gpl/bin/ffmpeg
      local url="https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linux64-gpl.tar.xz"
      echo "Downloading FFmpeg (BtbN static GPL) for Linux x86_64..."
      curl -fsSL "$url" -o "$tmpdir/ff.tar.xz"
      tar -xJf "$tmpdir/ff.tar.xz" -C "$tmpdir" --wildcards "*/bin/ffmpeg" --strip-components=2
      cp "$tmpdir/ffmpeg" "$dest"
      ;;
    aarch64-unknown-linux-gnu)
      # BtbN GPL static build pour Linux ARM64
      local url="https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linuxarm64-gpl.tar.xz"
      echo "Downloading FFmpeg (BtbN static GPL) for Linux ARM64..."
      curl -fsSL "$url" -o "$tmpdir/ff.tar.xz"
      tar -xJf "$tmpdir/ff.tar.xz" -C "$tmpdir" --wildcards "*/bin/ffmpeg" --strip-components=2
      cp "$tmpdir/ffmpeg" "$dest"
      ;;
    x86_64-apple-darwin|aarch64-apple-darwin)
      # evermeet.cx — recommandé par ffmpeg.org pour macOS.
      # ⚠ Intel x86_64 uniquement. Sur Apple Silicon, Rosetta 2 prend le relais.
      # Le format natif est .7z mais getrelease/ffmpeg/zip fournit un .zip.
      local url="https://evermeet.cx/ffmpeg/getrelease/ffmpeg/zip"
      echo "Downloading FFmpeg (evermeet.cx, Intel x86_64) for macOS..."
      echo "  Note: runs via Rosetta 2 on Apple Silicon (no native arm64 official source)"
      curl -fsSJL "$url" -o "$tmpdir/ff.zip"
      unzip -q "$tmpdir/ff.zip" -d "$tmpdir"
      cp "$tmpdir/ffmpeg" "$dest"
      # Ad-hoc sign for macOS
      if command -v codesign &>/dev/null; then
        codesign -s - "$dest" && echo "  ✓ Ad-hoc signed $dest"
      fi
      ;;
  esac

  chmod +x "$dest"
  echo "  ✓ $dest"
}

# ── Run ───────────────────────────────────────────────────────────────────────

# FFmpeg: sidecar on all platforms (official static build).
download_ffmpeg "$TRIPLE"

# MediaInfo: sidecar on macOS and Windows only.
# On Linux, mediainfo is declared as a .deb package dependency — the user's system
# provides it via apt. The sidecar_cmd() Rust function falls back to PATH automatically.
case "$OS" in
  Darwin)
    download_mediainfo "$TRIPLE"
    ;;
  Linux)
    echo "Linux: skipping mediainfo sidecar (declared as apt dependency in the .deb installer)."
    echo "  For local development, install it with: sudo apt install mediainfo"
    ;;
esac

echo
echo "Done. Files in src-tauri/binaries/:"
ls -lh "$BINARIES_DIR"
echo
echo "Next step: cd src-tauri && cargo build"
