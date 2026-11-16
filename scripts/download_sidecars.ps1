# download_sidecars.ps1 — Downloads Bartleby sidecar binaries (ffmpeg + mediainfo)
# from the project's GitHub Releases into src-tauri\binaries\.
#
# Usage (from project root, in PowerShell):
#   .\scripts\download_sidecars.ps1
#
# After running, rebuild with: cd src-tauri; cargo build

$ErrorActionPreference = "Stop"

# ── Where the binaries come from ──────────────────────────────────────────────
# RELEASE_URL points to a GitHub Release used ONLY as storage for the sidecars.
# It is NOT a Bartleby release: app versions are the v0.1.0-N tags.
# The tag name states exactly what it contains:
#   sidecars-ffmpeg-<ffmpeg version>-mediainfo-<mediainfo version>
# To update the sidecars: publish a NEW release named the same way with the 8
# bartleby-* files (+ SHA256SUMS), change RELEASE_URL here AND in
# download_sidecars.ps1, delete src-tauri/binaries/bartleby-* locally, re-run.
# Keep the old release: older Bartleby commits still download from it.
# Sources used for 9.0.2 / 26.10: see CLAUDE.md, section "Sidecars".
$RELEASE_URL = "https://github.com/colorgrain/Bartleby/releases/download/sidecars-ffmpeg-9.0.2-mediainfo-26.10"
$BINARIES_DIR = Join-Path $PSScriptRoot "..\src-tauri\binaries"
New-Item -ItemType Directory -Force -Path $BINARIES_DIR | Out-Null

Write-Host "Output: $BINARIES_DIR"
Write-Host ""

function Download-Sidecar($name) {
    $dest = Join-Path $BINARIES_DIR $name
    if (Test-Path $dest) {
        Write-Host "  (skip) $name already present"
        return
    }
    Write-Host "  v $name"
    Invoke-WebRequest -Uri "$RELEASE_URL/$name" -OutFile $dest -UseBasicParsing
}

Download-Sidecar "bartleby-ffmpeg-x86_64-pc-windows-msvc.exe"
Download-Sidecar "bartleby-mediainfo-x86_64-pc-windows-msvc.exe"

Write-Host ""
Write-Host "Done. Next step: cd src-tauri; cargo build"
