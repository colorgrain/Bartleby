# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is Bartleby

Bartleby is a multiplatform desktop application (Tauri v2) for film/media production data management: multi-destination file copy with MD5/XXH3 integrity verification, and optional metadata reports (CSV, PDF with thumbnails). Targeted at DITs (Digital Imaging Technicians).

## Commands

```bash
# Install JS dependencies (first time only)
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

**Release builds for distribution**: the `src-tauri/.cargo/cargo_config.toml` sets `target-cpu=native` (optimises for the local CPU's SIMD). Remove or override this flag for distributable packages so the binary runs on all CPUs:
```bash
cd src-tauri && RUSTFLAGS="" cargo build --release
```

## Architecture

Bartleby is a Tauri v2 application. No framework (React/Vue/Svelte) — the frontend is plain HTML/CSS/JS.

### Layer separation

```
src/                    ← Frontend (HTML/CSS/JS) — pure UI, no business logic
  index.html            ← Widget layout + inlined SVG icon sprites
  style.css             ← Base layout styles
  main.js               ← All UI logic: event wiring, IPC calls, state
  themes/               ← CSS skin files (mint-y-aqua, macos, windows11, adwaita)

src-tauri/src/          ← Rust backend
  main.rs               ← Tauri commands, AppState, thread orchestration
  copy_engine.rs        ← File transfer engine (3-phase: copy → verify → reports)
  metadata.rs           ← mediainfo CLI wrapper + CSV generation
  pdf_report.rs         ← printpdf PDF visual report generator
  settings.rs           ← Settings struct: JSON persistence via serde
```

### IPC bridge (JS ↔ Rust)

JS calls Rust via `invoke()`, Rust pushes to JS via `listen()` events:

| Direction | Mechanism | Used for |
|-----------|-----------|----------|
| JS → Rust | `invoke("command", args)` | Start copy, save settings, reply to prompts |
| Rust → JS | `win.emit("event", payload)` | Progress updates, log lines, copy-done, prompts |

All Tauri commands are in `main.rs`. Any new command must be added to the `generate_handler![]` list.

### Copy engine — 3-phase pipeline (`copy_engine.rs`)

1. **Phase 1 — Copy**: for each file sequentially, copies to all destinations in parallel (rayon). Uses `copy_file_range` (zero-copy kernel path). Calls `fsync` on each destination after copy.
2. **Phase 2 — Verify** (optional, when MD5 or XXH3 checked): re-reads source and all destinations with `O_DIRECT` (bypasses page cache for true end-to-end verification). MD5 via OpenSSL EVP / CommonCrypto / Windows CNG. XXH3 via `twox-hash`.
3. **Phase 3 — Reports**: writes `.md5`, `.xxh3`, `.csv`, `.pdf` as configured.

The copy engine runs on a dedicated OS thread (`std::thread::spawn`), never on Tokio's pool. A second forwarding thread translates `mpsc::Sender<Msg>` messages into Tauri events.

### Threading model

```
Tokio worker  → start_copy command handler (returns immediately)
  thread::spawn → copy_engine::run()    [I/O, blocks for full duration]
                      │  mpsc Sender<Msg>
  thread::spawn → forwarding loop  →  win.emit(…) to JavaScript
```

### Interactive prompts during copy

When the copy engine encounters a non-empty destination or file conflicts, it blocks on `reply_rx.recv()` and sends a `Msg::NonEmptyDest` or `Msg::Conflicts`. The forwarding thread emits `"copy-prompt"` to JS, which shows a modal. The user's response calls `invoke("prompt_reply")`, which sends on `AppState.reply_tx` to unblock the engine.

### Settings (`settings.rs`)

`Settings` struct is `Serialize + Deserialize + Clone`. Persisted at:
- Linux: `~/.config/bartleby/settings.json`
- macOS: `~/Library/Application Support/bartleby/settings.json`
- Windows: `%APPDATA%\bartleby\settings.json`

`#[serde(default)]` on the struct ensures forward compatibility: fields absent from older settings files are filled from `Default` without error.

### Appearance system (two orthogonal axes)

- **Skin** (`settings.skin`): which CSS file to load — `mint-y-aqua` | `macos` | `windows11` | `adwaita`. JS swaps `<link id="theme-link">` href.
- **Theme** (`settings.theme`): `light` | `dark` | `default` (follow OS). JS sets `body.className`. On Linux, `prefers-color-scheme` is unreliable inside Tauri WebKit; the `is_system_dark_mode` Rust command uses `gsettings` / `GTK_THEME` env var instead.

## Key constraints

- **Folder picker is JS-only**: calling `tauri_plugin_dialog`'s `pick_folder` from a Rust worker thread deadlocks on Linux (GTK main thread conflict). The dialog is always opened from JS via `window.__TAURI__.dialog.open()`.
- **`printpdf` version is pinned at `0.7`**: the polygon/line API changed across minor versions. Upgrading requires careful review of all drawing calls in `pdf_report.rs`.
- **`mediainfo` is a runtime dependency**: if not installed, metadata fields are empty strings. Copy and verification are unaffected.
- **Linux notifications**: `tauri_plugin_notification` can fail silently on Cinnamon. The `send_notification` command uses `notify-send` CLI directly on Linux.
- **`var` in `main.js`**: intentional — compatibility with older WebKit versions on Linux.
