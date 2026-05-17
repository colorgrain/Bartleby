# download_sidecars.ps1 — Downloads Bartleby sidecar binaries (ffmpeg + mediainfo)
# from the project's GitHub Releases into src-tauri\binaries\.
#
# Usage (from project root, in PowerShell):
#   .\scripts\download_sidecars.ps1
#
# After running, rebuild with: cd src-tauri; cargo build

$ErrorActionPreference = "Stop"

$RELEASE_URL = "https://github.com/colorgrain/Bartleby/releases/download/binaries-v1"
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
