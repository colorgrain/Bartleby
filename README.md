# Bartleby
<img width="128" height="128" alt="128x128" src="https://github.com/user-attachments/assets/82851c10-9c74-4bb5-ba11-b6f00c435657" />


<img width="2200" height="1464" alt="Bartleby 010-3_1" src="https://github.com/user-attachments/assets/c19d7ea2-f891-424d-aad4-456abb4ab19d" />
<img width="2200" height="1464" alt="Bartleby 010-3_2" src="https://github.com/user-attachments/assets/343877f5-6367-4f56-83f3-cc4c79d175f3" />
<img width="2200" height="1464" alt="Bartleby 010-3_3" src="https://github.com/user-attachments/assets/81668e62-6dcc-4d54-b1d3-ccbacc18d4c6" />
<img width="2200" height="1464" alt="Bartleby 010-3_4" src="https://github.com/user-attachments/assets/35c0ccec-39fe-4c26-89f9-70cd9209a372" />
<img width="2200" height="1464" alt="Bartleby 010-3_5" src="https://github.com/user-attachments/assets/06dc2b6c-c1ec-44c8-81b1-c266c70deee5" />
<img width="2200" height="1464" alt="Bartleby 010-3_6" src="https://github.com/user-attachments/assets/66b51ea7-7366-4061-9b89-18aa99b3e51a" />
<img width="1768" height="1256" alt="Bartleby 010-3_7" src="https://github.com/user-attachments/assets/2f386321-781c-4aae-914e-d19b8e0872ec" />




> **Beta software** — Bartleby is under active development. Expect rough edges, missing features, and breaking changes between releases. Use at your own risk in production workflows.

Bartleby is a desktop application for film and media production data management, designed primarily for **DITs (Digital Imaging Technicians)**. It handles multi-destination file offloading with end-to-end integrity verification (MD5, SHA-1, XXH3, and more), generates optional metadata reports (CSV, HTML, PDF with thumbnails), and produces ASC MHL v2.0 hash lists. A built-in verification tool lets you audit any checksum or MHL file independently.

Built with [Tauri v2](https://tauri.app/) (Rust backend + plain HTML/CSS/JS frontend), Bartleby runs natively on Linux, macOS, and Windows from a single codebase, and is designed to look as native as possible on each platform. Developed with the assistance of [Claude Code](https://claude.ai/code).

---

## Features

- **Multi-destination copy** with parallel writes to all destinations simultaneously
- **End-to-end integrity verification**: MD5 · SHA-1 · XXH64 · XXH3-64 · XXH128 · C4 ID
- **ASC MHL v2.0** hash list generation and multi-generation chain management
- **Standalone verification window**: verify any checksum or MHL file with live per-file results, pause/resume/cancel, HTML report export, and post-verification MHL generation
- **Metadata reports**: CSV table, self-contained HTML report, PDF with thumbnails — all optional, configurable per job
- **Multi-job queue**: run several independent copy jobs in sequence, each with its own source, destinations, and options
- **Conflict detection**: interactive prompts for non-empty destinations and file conflicts, with size and date comparison
- **Live progress**: per-job progress bar with transfer speed, ETA, and current filename
- **Light / dark mode** and multiple UI skins (Mint-Y Aqua, Adwaita, macOS, Windows 11)
- **Native window decorations** synced to the active theme on all platforms

---

## Installing a pre-built binary

Download the installer for your platform from the [Releases](../../releases) page.

mediainfo and ffmpeg are bundled in all installers — no separate installation is required.

### Linux

Install the `.deb` package (Debian/Ubuntu) or run the `.AppImage` directly:

```bash
# .deb
sudo dpkg -i bartleby_*.deb

# .AppImage
chmod +x Bartleby_*.AppImage && ./Bartleby_*.AppImage
```

### macOS

Open the `.dmg`, drag **Bartleby** to `/Applications`. On first launch, right-click the app → **Open** to bypass Gatekeeper (the app is not notarised yet).

### Windows

Run the `.msi` installer and follow the prompts.

---

## Building from source

### Prerequisites — all platforms

- [Rust](https://rustup.rs/) stable toolchain
- [Node.js](https://nodejs.org/) 18 or later

### Linux (Ubuntu / Debian)

```bash
sudo apt install libwebkit2gtk-4.1-dev libssl-dev librsvg2-dev \
  libgtk-3-dev pkg-config build-essential
```

### macOS

```bash
xcode-select --install
```

### Windows

WebView2 runtime is pre-installed on Windows 11. For Windows 10, download it from Microsoft.

### Download sidecar binaries

Before the first build, download the bundled mediainfo and ffmpeg binaries:

```bash
# Linux / macOS
bash scripts/download_sidecars.sh

# Windows
.\scripts\download_sidecars.ps1
```

### Build commands

```bash
# Install JS dependencies (first time only)
npm install

# Development server with hot-reload
npm run dev

# Distributable installer
npm run build
```

> **Note for distribution builds**: `src-tauri/.cargo/cargo_config.toml` sets `target-cpu=native` to enable local SIMD optimisations. Override it when building for distribution so the binary runs on all CPUs:
> ```bash
> cd src-tauri && RUSTFLAGS="" cargo build --release
> ```

---

## Contributing

Contributions are welcome — bug reports, feature requests, pull requests, and feedback from working DITs all help.

- **Bug reports / feature requests**: open an issue and describe the problem or use case
- **Code contributions**: fork the repo, make your changes on a branch, and open a pull request with a clear description of what changed and why
- **On-set feedback**: if you use Bartleby in a real production context, your experience is valuable — please share it in the issues

The codebase is intentionally straightforward: the frontend is plain HTML/CSS/JS, the backend is Rust with Tauri v2. There is no framework to learn.
