# Bartleby

> **Beta software** — Bartleby is under active development. Expect rough edges, missing features, and breaking changes between releases. Use at your own risk in production workflows.

Bartleby is a desktop application designed for people who regularly need to copy files to multiple desinations and check their intergrity. I particularly had in mind film and media production data management, and the **DITs (Digital Imaging Technicians)**. It handles multi-destination file offloading with MD5 and XXH3 integrity verification, and generates optional metadata reports (CSV, HTML, PDF with thumbnails). Reports can be customized in the settings.

Built with [Tauri v2](https://tauri.app/) (Rust backend + plain HTML/CSS/JS frontend), Bartleby runs natively on Linux, macOS, and Windows from a single codebase. Is was designed to look as native as possible on all platforms. It was developed with the assistance of [Claude Code](https://claude.ai/code), Anthropic's AI coding tool.

---

## Features

- Multi-destination copy with parallel writes
- End-to-end integrity verification (MD5 · XXH3)
- Optional metadata reports: CSV, HTML, PDF with thumbnails
- Conflict detection and interactive resolution prompts
- Light / dark mode, multiple UI skins

---

## Installing a pre-built binary

Download the installer for your platform from the [Releases](../../releases) page.

### Linux

Install the `.deb` package (Debian/Ubuntu) or run the `.AppImage` directly:

```bash
# .deb
sudo dpkg -i bartleby_*.deb

# .AppImage
chmod +x Bartleby_*.AppImage && ./Bartleby_*.AppImage
```

Runtime dependencies (copy and verification work without them, but metadata reports require both):

```bash
sudo apt install mediainfo ffmpeg
```

### macOS

Open the `.dmg`, drag **Bartleby** to `/Applications`, then:

```bash
brew install mediainfo ffmpeg
```

### Windows

Run the `.msi` installer.

Download and add to `PATH`:
- [MediaInfo CLI](https://mediaarea.net/en/MediaInfo/Download/Windows)
- [FFmpeg](https://ffmpeg.org/download.html)

---

## Building from source

### Prerequisites — all platforms

- [Rust](https://rustup.rs/) (stable toolchain)
- [Node.js](https://nodejs.org/) 18 or later

### Linux (Ubuntu / Debian)

```bash
sudo apt install libwebkit2gtk-4.1-dev libssl-dev librsvg2-dev \
  pkg-config build-essential mediainfo ffmpeg
```

### macOS

```bash
xcode-select --install
brew install mediainfo ffmpeg
```

### Windows

- WebView2 runtime (pre-installed on Windows 11; download from Microsoft for Windows 10)
- [MediaInfo CLI](https://mediaarea.net/en/MediaInfo/Download/Windows) → added to `PATH`
- [FFmpeg](https://ffmpeg.org/download.html) → added to `PATH`

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

# Correction déclencheur
