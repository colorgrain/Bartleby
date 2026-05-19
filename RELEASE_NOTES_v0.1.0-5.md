# Bartleby v0.1.0-5 — Release Notes

This release covers all changes since **v0.1.0-3**. It includes two milestones: the v0.1.0-4 feature release (MHL, verification engine, bundled sidecars) and the v0.1.0-5 polish and bugfix pass.

---

## What's new since v0.1.0-3

### ASC MHL v2.0 hash lists

Bartleby can now generate **ASC MHL v2.0** (`.mhl`) files alongside each copy — the professional standard for file manifests on film and broadcast productions. The MHL records the hash of every transferred file in an XML structure that any MHL-compatible tool can read and verify.

Each generation is tracked: if a destination already contains an MHL from a previous transfer, Bartleby detects it and prompts you to replace or keep both (the new MHL is written as generation N+1, preserving the chain).

### Standalone verification window

A dedicated **verification tool** is now built into Bartleby. Open it from the main window with the shield button. It accepts:

- `.md5`, `.sha1`, `.xxh64`, `.xxh3`, `.xxh128`, `.c4` flat checksum files
- `.mhl` (ASC MHL v2.0) — full chain display with per-generation metadata

The file list loads immediately on open (before verification starts) so you can review what will be checked. Each file's status updates live as verification proceeds. Controls: pause, resume, cancel at any point.

At the end of a verification pass, you can:
- Save an **HTML report** of the results
- Generate a **post-verification MHL** (process type `verify`, generation N+1) for the audited MHL

### Bundled mediainfo and ffmpeg sidecars

Pre-built installers now include **bartleby-mediainfo** and **bartleby-ffmpeg** as bundled binaries. No manual installation of mediainfo or ffmpeg is required to generate metadata reports, thumbnails, or waveform images. If system-installed versions are present and no bundled binary is found, they are used as a fallback.

### Live progress ticker during copy

A ticker beside the progress bar now shows the **current transfer speed** (MB/s), an estimated time remaining, and the active filename. Previously the label only showed the filename.

### Multi-job queue

You can now queue **multiple independent copy jobs** in a single session. Each job has its own source, destinations, hash algorithm, and report options. Jobs run sequentially; a per-job progress bar shows the status of each. A summary line is appended to the log at the end of each job.

### Window decoration theme — native OS chrome

The native window title bar now matches the active Bartleby theme on all platforms:

- **Linux (Cinnamon / GNOME)**: the `_GTK_THEME_VARIANT` X11 property is set on each window so Muffin/Mutter uses the correct light or dark decoration variant. The `GTK_THEME` environment variable is also set before Tauri initialises GTK, so the border matches the saved theme from the very first frame.
- **macOS / Windows**: Tauri's `set_theme()` API propagates the light/dark preference to the native window chrome.

---

## Bug fixes

- **Windows**: fixed a crash when opening the verification window — `set_window_theme` was being called on a hidden WebView2 window before it was made visible, causing WebView2 to terminate. The call is now deferred until after `show_verifier_window` resolves.
- **Linux**: eliminated a white zone at the top of the verifier window on Linux Mint / Cinnamon.
- **Linux**: the verifier window is created hidden and shown only after the theme is applied, preventing a white flash on first open.
- **Linux**: the main window decoration correctly picks up light/dark at launch without requiring a theme toggle.
- **macOS**: mediainfo is correctly located when installed via Homebrew (`/opt/homebrew/bin` injected into the sidecar search path).
- **Windows**: `DRIVE_*` constants defined locally for compatibility with `windows-sys 0.52`.
- **About modal**: version string is now read at runtime from the binary via `get_app_version`, so it is always in sync with `Cargo.toml` without manual updates.

---

## Supported hash algorithms

| Algorithm | Output file | Notes |
|-----------|-------------|-------|
| None | — | Copy only, no checksum |
| Size only | — | Size comparison only |
| MD5 | `.md5` | Compatible with `md5sum -c` |
| SHA-1 | `.sha1` | Compatible with `sha1sum -c` |
| XXH64 | `.xxh64` | Via `xxhsum` |
| XXH3-64 | `.xxh3` | Via `xxhsum` |
| XXH128 | `.xxh128` | Via `xxhsum` |
| C4 ID | `.c4` | Content-addressable identifier |

MHL generation is available for all algorithms except None and Size.

---

## Installing

Download the installer for your platform from the [Releases](../../releases) page.

**Linux**: `.deb` (Debian/Ubuntu) or `.AppImage`

```bash
sudo dpkg -i bartleby_*.deb
# or
chmod +x Bartleby_*.AppImage && ./Bartleby_*.AppImage
```

**macOS**: `.dmg` — drag to `/Applications`, right-click → Open on first launch to bypass Gatekeeper.

**Windows**: `.msi` installer — run and follow the prompts.

mediainfo and ffmpeg are bundled in all installers. No separate installation required.

---

> **Beta software** — Bartleby is under active development. Back up your data independently of any copy tool.
