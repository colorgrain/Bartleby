# download_sidecars.ps1 — Downloads MediaInfo CLI and FFmpeg for Windows x86_64
# and places them in src-tauri\binaries\ with the Tauri sidecar naming convention.
#
# Usage (from project root, in PowerShell):
#   .\scripts\download_sidecars.ps1
#
# After running, rebuild with:
#   cd src-tauri; cargo build     # or npm run build

$ErrorActionPreference = "Stop"

$TRIPLE = "x86_64-pc-windows-msvc"
$BINARIES_DIR = Join-Path $PSScriptRoot "..\src-tauri\binaries"
$MEDIAINFO_VERSION = "26.05"

New-Item -ItemType Directory -Force -Path $BINARIES_DIR | Out-Null
Write-Host "Output dir: $BINARIES_DIR"

# ── MediaInfo CLI ─────────────────────────────────────────────────────────────
# Official: https://mediaarea.net/en/MediaInfo/Download/Windows

$miDest = Join-Path $BINARIES_DIR "mediainfo-$TRIPLE.exe"
if (Test-Path $miDest) {
    Write-Host "mediainfo-$TRIPLE.exe already present, skipping."
} else {
    $miUrl  = "https://mediaarea.net/download/binary/mediainfo/$MEDIAINFO_VERSION/MediaInfo_CLI_${MEDIAINFO_VERSION}_Windows_x64.zip"
    $miZip  = Join-Path $env:TEMP "mediainfo.zip"
    $miTmp  = Join-Path $env:TEMP "mediainfo_extract"
    Write-Host "Downloading MediaInfo $MEDIAINFO_VERSION for Windows x64..."
    Invoke-WebRequest -Uri $miUrl -OutFile $miZip -UseBasicParsing
    Expand-Archive -Path $miZip -DestinationPath $miTmp -Force
    $miBin = Get-ChildItem -Path $miTmp -Filter "MediaInfo.exe" -Recurse | Select-Object -First 1
    Copy-Item $miBin.FullName $miDest
    Remove-Item $miZip, $miTmp -Recurse -Force
    Write-Host "  OK $miDest"
}

# ── FFmpeg ────────────────────────────────────────────────────────────────────
# Official: https://www.gyan.dev/ffmpeg/builds/
# Using the "essentials" build which contains only ffmpeg/ffprobe/ffplay.

$ffDest = Join-Path $BINARIES_DIR "ffmpeg-$TRIPLE.exe"
if (Test-Path $ffDest) {
    Write-Host "ffmpeg-$TRIPLE.exe already present, skipping."
} else {
    $ffUrl  = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip"
    $ffZip  = Join-Path $env:TEMP "ffmpeg.zip"
    $ffTmp  = Join-Path $env:TEMP "ffmpeg_extract"
    Write-Host "Downloading FFmpeg for Windows x64..."
    Invoke-WebRequest -Uri $ffUrl -OutFile $ffZip -UseBasicParsing
    Expand-Archive -Path $ffZip -DestinationPath $ffTmp -Force
    $ffBin = Get-ChildItem -Path $ffTmp -Filter "ffmpeg.exe" -Recurse | Select-Object -First 1
    Copy-Item $ffBin.FullName $ffDest
    Remove-Item $ffZip, $ffTmp -Recurse -Force
    Write-Host "  OK $ffDest"
}

Write-Host ""
Write-Host "Done. Files in src-tauri\binaries\:"
Get-ChildItem $BINARIES_DIR | Format-Table Name, Length, LastWriteTime
Write-Host ""
Write-Host "Next step: cd src-tauri; cargo build"
