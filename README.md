# Bartleby
<img width="128" height="128" alt="128x128" src="https://github.com/user-attachments/assets/82851c10-9c74-4bb5-ba11-b6f00c435657" />

<img width="1440" height="1830" alt="Capture d’écran du 2026-05-03 09-01-04" src="https://github.com/user-attachments/assets/6226c31b-f898-479b-b05b-be885ef063b2" />
<img width="1440" height="1718" alt="Capture d’écran du 2026-05-03 09-03-39" src="https://github.com/user-attachments/assets/2993fc88-de99-492b-9820-ff4645af3bdc" />
<img width="1440" height="1718" alt="Capture d’écran du 2026-05-03 09-03-28" src="https://github.com/user-attachments/assets/afe720f4-e58e-4d76-a749-508e846404ef" />
<img width="1440" height="1424" alt="Capture d’écran du 2026-05-03 09-02-41" src="https://github.com/user-attachments/assets/f014eb7e-d0ac-4863-b781-f7591b71ef44" />
<img width="1440" height="1830" alt="Capture d’écran du 2026-05-03 09-02-20" src="https://github.com/user-attachments/assets/644c2ee8-a993-4c58-b886-2eec52e9eb8b" />



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
