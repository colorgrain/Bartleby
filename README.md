# Bartleby
<img width="128" height="128" alt="128x128" src="https://github.com/user-attachments/assets/2e68bce0-2450-491d-8247-1d56ef56f638" />

<img width="1825" height="1095" alt="Capture d&#39;écran_20261117_144146" src="https://github.com/user-attachments/assets/fdc9d2f1-d52c-41cb-8a80-93b20f1e8493" />
<img width="1225" height="875" alt="Capture d&#39;écran_20261117_144255" src="https://github.com/user-attachments/assets/5bd15524-0107-4632-862e-7a64997dd458" />
<img width="1825" height="1095" alt="Capture d&#39;écran_20261117_144431" src="https://github.com/user-attachments/assets/be688a87-613f-47a5-9240-e962b4115e5e" />
<img width="1825" height="1095" alt="Capture d&#39;écran_20261117_144423" src="https://github.com/user-attachments/assets/9768e1bf-433f-4329-b0d2-b0e65fa6f335" />
<img width="1825" height="1095" alt="Capture d&#39;écran_20261117_144405" src="https://github.com/user-attachments/assets/abe45fd9-9f36-44e9-b79b-df191d970333" />
<img width="1825" height="1095" alt="Capture d&#39;écran_20261117_144356" src="https://github.com/user-attachments/assets/b10ebee6-fabe-4604-9356-8d4b2ea7ccd5" />
<img width="1825" height="1095" alt="Capture d&#39;écran_20261117_144347" src="https://github.com/user-attachments/assets/aaedfd25-5711-42c9-a78b-69833a2b62d7" />
<img width="1825" height="1095" alt="Capture d&#39;écran_20261117_144319" src="https://github.com/user-attachments/assets/43209d1e-da4a-44a6-ba53-d73ae1161bc2" />


> **Beta software** — Bartleby is under active development. Expect rough edges, missing features, and breaking changes between releases. Use at your own risk in production workflows.

Bartleby is a desktop application for film and media production data management, designed primarily for **DITs (Digital Imaging Technicians)**. It handles multi-destination file offloading with end-to-end integrity verification (MD5, SHA-1, XXH3, and more), generates optional metadata reports (CSV, HTML, PDF with thumbnails and camera colour metadata), and produces ASC MHL v2.0 histories. A built-in verification tool lets you audit any checksum or MHL file independently.

Built with [Tauri v2](https://tauri.app/) (Rust backend + plain HTML/CSS/JS frontend), Bartleby runs natively on Linux, macOS, and Windows from a single codebase, and is designed to look as native as possible on each platform. Developed with the assistance of [Claude Code](https://claude.ai/code).

---

## Features

- **Multi-destination copy** with parallel writes to all destinations simultaneously
- **End-to-end integrity verification**: MD5 · SHA-1 · XXH64 · XXH3-64 · XXH128 · C4 ID — every copy is re-read **from the disk** (page cache bypassed), and a job is only reported successful when every file is complete and matching on every destination
- **Checksum files** compatible with `md5sum -c`, `sha1sum -c` and `xxhsum -c`
- **ASC MHL v2.0 histories** that travel with the card (`{card}/ascmhl/`): generations, `ascmhl_chain.xml`, append-only; a card's existing history is continued (`verified` / `failed` / `original`). Output validated with the reference `ascmhl` tool
- **Standalone verification window**: verify any checksum or MHL file with live per-file results, pause/resume/cancel, HTML report export, and post-verification MHL generation
- **Metadata reports**: CSV, self-contained HTML, PDF with thumbnails — all optional, configurable per job; full hashes, Unicode file names, camera gamut / log read from MXF, Sony XML and RED R3D metadata
- **Safety guards**: a destination equal to (or inside) the source is refused, existing files are never silently overwritten, skipped items are listed, partial files are removed after an error
- **Multi-job queue**: run several independent copy jobs in sequence, each with its own source, destinations, and options — with a per-job status badge (idle / running / done / failed)
- **Conflict detection**: interactive prompts for non-empty destinations and file conflicts, with size and date comparison
- **Disk explorer**: browse the machine's mounted volumes, auto-refreshed as media is plugged in or unplugged
- **Live progress**: per-job progress bar with per-disk transfer speed, phase-aware ETA, and current filename
- **Window styles** (Breeze, Adwaita, macOS, Windows) with light / dark mode and custom colour themes

---

## Installing a pre-built binary

Download the installer for your platform from the [Releases](../../releases) page.

mediainfo and ffmpeg are bundled in all installers — no separate installation is required.

### Linux

Install the `.deb` (Debian/Ubuntu), `.rpm` (Fedora/openSUSE) or Arch package, or run the `.AppImage` directly:

```bash
# .deb
sudo dpkg -i Bartleby_*.deb

# .rpm
sudo rpm -i Bartleby-*.rpm

# Arch Linux
sudo pacman -U Bartleby_*_x86_64.pkg.tar.zst

# .AppImage
chmod +x Bartleby_*.AppImage && ./Bartleby_*.AppImage
```

### macOS (Apple Silicon and Intel)

With [Homebrew](https://brew.sh) — the app is not notarised, so install without quarantine:

```bash
brew tap colorgrain/bartleby
brew install --cask --no-quarantine bartleby
```

Or open the universal `.dmg` and drag **Bartleby** to `/Applications`. On first launch, right-click the app → **Open** to bypass Gatekeeper.

### Windows

Run the `.msi` or `-setup.exe` installer and follow the prompts.

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

> **Note for distribution builds**: never ship a binary built with `target-cpu=native` (see the inactive `src-tauri/.cargo/cargo_config.toml`, meant for local benchmarking only). Distribution builds use an empty `RUSTFLAGS`:
> ```bash
> RUSTFLAGS="" npm run build
> ```
>
> On Arch-based systems, building the AppImage locally needs `NO_STRIP=true` (linuxdeploy's bundled `strip` does not understand recent binaries).

---

## Contributing

Contributions are welcome — bug reports, feature requests, pull requests, and feedback from working DITs all help.

- **Bug reports / feature requests**: open an issue and describe the problem or use case
- **Code contributions**: fork the repo, make your changes on a branch, and open a pull request with a clear description of what changed and why
- **On-set feedback**: if you use Bartleby in a real production context, your experience is valuable — please share it in the issues

The codebase is intentionally straightforward: the frontend is plain HTML/CSS/JS, the backend is Rust with Tauri v2. There is no framework to learn.
