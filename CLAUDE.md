# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Bartleby

Bartleby is a multiplatform desktop application (Tauri v2) for film/media production data management, targeted at DITs (Digital Imaging Technicians):

- Multi-destination file copy with integrity verification (MD5, SHA-1, XXH64, XXH3, XXH128, C4, or size-only).
- Checksum sidecar files, ASC MHL v2.0 hash lists, and optional metadata reports (CSV, PDF, HTML with thumbnails).
- A separate **Verifier** window that re-checks existing checksum files / MHL against the media.

## Commands

```bash
# Download sidecar binaries (bundled ffmpeg + mediainfo) — required before first build
bash scripts/download_sidecars.sh
# Windows: .\scripts\download_sidecars.ps1

# Install JS dependencies (provides the `tauri` CLI)
npm install

# Development with hot-reload
npm run dev

# Build distributable installer
npm run build

# Compile Rust backend only (faster iteration)
cd src-tauri && cargo build

# Run Rust tests
cd src-tauri && cargo test

# Check Rust code without linking
cd src-tauri && cargo check
```

**`target-cpu=native`**: `src-tauri/.cargo/cargo_config.toml` documents this flag, but it is currently **inactive** — Cargo only reads `.cargo/config.toml`, and the `rustflags` line is commented out anyway. To enable it for local benchmarking, rename the file to `config.toml` and uncomment `[build]` / `rustflags`. Never ship binaries built with it; distribution builds use `RUSTFLAGS=""` (see `packaging/arch/PKGBUILD`).

## Architecture

Bartleby is a Tauri v2 application. No framework (React/Vue/Svelte) — the frontend is plain HTML/CSS/JS.

### Layer separation

```
src/                    ← Frontend (HTML/CSS/JS) — pure UI, no business logic
  index.html            ← Main window layout + inlined SVG icon sprites
  main.js               ← Main window UI logic: event wiring, IPC calls, state
  appearance.js         ← Shared by both windows: drawn title bar/buttons/resize edges + colour themes
  verifier.html/.js     ← Verifier window (checksum / MHL verification)
  style.css             ← Base layout styles
  themes/               ← Palette per window style: adwaita.css, breeze.css, macos.css, windows.css

src-tauri/src/          ← Rust backend
  main.rs               ← Tauri commands, AppState, thread orchestration, window setup
  copy_engine.rs        ← File transfer engine (3-phase: copy → verify → reports)
  verify_engine.rs      ← Parses .md5/.sha1/.xxh*/.c4/.mhl files and re-hashes media
  mhl_report.rs         ← ASC MHL v2.0 writer (generational chain in {dst}/ascmhl/)
  metadata.rs           ← mediainfo CLI wrapper + CSV generation
  camera_meta.rs        ← Vendor camera metadata mediainfo misses: MXF gamut/log labels, Sony XML sidecar, R3D header
  pdf_report.rs         ← printpdf PDF visual report generator
  html_report.rs        ← Self-contained HTML report (inlined CSS + base64 thumbnails)
  sidecar.rs            ← Resolves bundled bartleby-ffmpeg / bartleby-mediainfo, PATH fallback
  settings.rs           ← Settings struct: JSON persistence via serde

src-tauri/binaries/     ← Sidecar binaries (git-ignored, see download_sidecars.sh)
src-tauri/capabilities/ ← default.json (main window), verifier.json (verifier window)
packaging/arch/PKGBUILD ← Arch package, built in CI from the .deb bundle
.github/workflows/release.yml ← Tagged release: Linux/Windows/macOS matrix + Arch package
```

### IPC bridge (JS ↔ Rust)

JS calls Rust via `invoke()`, Rust pushes to JS via `listen()` events:

| Direction | Mechanism | Used for |
|-----------|-----------|----------|
| JS → Rust | `invoke("command", args)` | Start/pause/cancel copy or verification, settings, prompt replies |
| Rust → JS | `win.emit("event", payload)` | Progress updates, log lines, copy-done, prompts |

All Tauri commands are in `main.rs`. Any new command must be added to the `generate_handler![]` list. A new window also needs its own capability file in `src-tauri/capabilities/`.

### Copy engine — 3-phase pipeline (`copy_engine.rs`)

1. **Phase 1 — Copy**: for each file sequentially, copies to all destinations in parallel (rayon) using `copy_file_range` (zero-copy kernel path). Flushes with `syncfs()` once per filesystem after all files (Linux) or `sync_all()` (other platforms).
2. **Phase 2 — Verify** (when `hash_algo != "none"`): re-reads source and all destinations with `O_DIRECT` (Linux) / `F_NOCACHE` (macOS) / `FILE_FLAG_NO_BUFFERING` (Windows) to bypass the page cache. On FUSE filesystems (ntfs-3g, exfat-fuse, sshfs) O_DIRECT fails, so `posix_fadvise(DONTNEED)` + buffered read is used instead.
3. **Phase 3 — Reports**: checksum sidecar file, MHL, `.csv`, `.pdf`, `.html` as configured.

Hash algorithms (`HashAlgo` enum, one per job): `none`, `size`, `md5` (OpenSSL EVP / CommonCrypto / Windows CNG), `sha1`, `xxh64`, `xxh3` (64-bit), `xxh128`, `c4` (SHA-512 + base58).

The copy engine runs on a dedicated OS thread (`std::thread::spawn`), never on Tokio's pool. A second forwarding thread translates `mpsc::Sender<Msg>` messages into Tauri events. The verification engine follows the same pattern.

### Threading model

```
Tokio worker  → start_copy command handler (returns immediately)
  thread::spawn → copy_engine::run()    [I/O, blocks for full duration]
                      │  mpsc Sender<Msg>
  thread::spawn → forwarding loop  →  win.emit(…) to JavaScript
```

### Interactive prompts during copy

When the copy engine encounters a non-empty destination or file conflicts, it sends `Msg::NonEmptyDest` or `Msg::Conflicts` and blocks on `reply_rx.recv()`. The forwarding thread emits `"copy-prompt"` to JS, which shows a modal. The user's response calls `invoke("prompt_reply")`, which sends on `AppState.reply_tx` to unblock the engine.

### Verifier window

Created hidden at startup (label `verifier`, `verifier.html`) and shown on demand by `open_verifier_window`; closing hides it rather than destroying it. Commands: `parse_verification_file`, `start_verification`, `pause/resume/cancel_verification`, `save_verify_html`, `generate_post_verify_mhl` (writes a next-generation MHL with `<process>verify</process>`).

### Sidecars (`sidecar.rs`)

ffmpeg (video thumbnails, audio waveforms) and mediainfo (metadata) are bundled as Tauri `externalBin` (`bartleby-ffmpeg`, `bartleby-mediainfo`). `sidecar.rs` looks next to the executable first, then falls back to `ffmpeg` / `mediainfo` on PATH (on macOS, Homebrew dirs are added to PATH). The binaries are downloaded from the `sidecars-ffmpeg-9.0.2-mediainfo-26.10` GitHub release: FFmpeg 9.0.2 (martin-riedl.de builds for Linux/macOS, gyan.dev full build for Windows) and MediaInfo CLI 26.10 (MediaArea; Linux = the `Lambda` build, glibc ≥ 2.25). The Linux ffmpeg needs glibc ≥ 2.35, i.e. the same Ubuntu 22.04 baseline as the CI build. Thumbnail extraction goes through `sidecar::extract_video_thumbnail`, which retries with BT.709 colour tags (FFmpeg 9 rejects the invalid tags of ARRI MXF) and without the 1 s seek (clips shorter than 1 s).

### Settings (`settings.rs`)

`Settings` struct is `Serialize + Deserialize + Clone`. Persisted at:
- Linux: `~/.config/bartleby/settings.json`
- macOS: `~/Library/Application Support/bartleby/settings.json`
- Windows: `%APPDATA%\bartleby\settings.json`

The log file `bartleby.log` lives next to it.

`#[serde(default)]` on the struct ensures forward compatibility: fields absent from older settings files are filled from `Default` without error.

### Window chrome & appearance (`src/appearance.js`)

Both windows are **undecorated and transparent** (`decorations: false`, `transparent: true` in `tauri.conf.json` and the verifier's `WebviewWindowBuilder`). The OS draws nothing: `appearance.js` turns `#titlebar` into a `data-tauri-drag-region`, adds minimise/maximise/close buttons and invisible resize edges (`startResizeDragging`), and `#app-shell` draws the rounded frame (square when maximised). macOS transparency needs `macOSPrivateApi: true` + the tauri `macos-private-api` feature. Window permissions (`allow-start-dragging`, `allow-minimize`, `allow-hide`…) are listed in both capability files.

- **Window style** (`settings.skin`): `adwaita` | `breeze` | `macos` | `windows` (default on Linux: Breeze under KDE, libadwaita otherwise). Sets `body[data-os]` (button layout, corner radius — CSS at the end of `style.css`) and loads `themes/{skin}.css` (palette). Old values (`linux`, `mint-y-aqua`, `windows11`) are migrated by `settings::migrate_skin` and `normalizeSkin()` in JS.
- **Colour theme** (`settings.theme`): `default` (follow OS) | `light` | `dark` | `custom:<name>`. Custom themes (`settings.custom_themes`) store background / foreground (panels, cards) / text / accent; `derivePalette()` computes every other CSS token and sets them inline on `<body>`. On Linux, `prefers-color-scheme` is unreliable inside WebKitGTK, so "follow OS" uses the `is_system_dark_mode` Rust command (gsettings / `GTK_THEME`).
- `set_window_theme` only calls `win.set_theme()` (native dialogs, WebView colour scheme) — there is no native title bar to style any more.
- The main window broadcasts `appearance-changed` after saving; the verifier re-reads settings on it and on `verifier-shown`.
- Main window close button → `window.close()` → Rust `CloseRequested` → `app-close-requested` JS guard (warns during a copy). Verifier close button → `hide()` (the window is created once at startup and reused).

## Versioning

The version must be kept in sync in `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json` (format `0.1.0-N`) and `packaging/arch/PKGBUILD` (`pkgver=0.1.0.N`, also set by CI). Release notes go in `RELEASE_NOTES_v<version>.md`.

## Key constraints

- **Folder picker is JS-only**: calling `tauri_plugin_dialog`'s `pick_folder` from a Rust worker thread deadlocks on Linux (GTK main thread conflict). The dialog is always opened from JS via `window.__TAURI__.dialog.open()`.
- **`printpdf` version is pinned at `0.7`**: the polygon/line API changed across minor versions. Upgrading requires careful review of all drawing calls in `pdf_report.rs`.
- **Missing sidecars degrade gracefully**: without mediainfo, metadata fields are empty strings; without ffmpeg, video/audio thumbnails fall back to icons. Copy and verification are unaffected.
- **Linux notifications**: `tauri_plugin_notification` can fail silently on Cinnamon. The `send_notification` command uses `notify-send` CLI directly on Linux.
- **`var` in `main.js`**: intentional — compatibility with older WebKit versions on Linux.
