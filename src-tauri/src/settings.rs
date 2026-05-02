//! # Module `settings`
//!
//! Centralises all user-configurable preferences for Bartleby into a single
//! JSON-serialisable struct. This module is the only place where settings are
//! defined, stored, loaded, and persisted.
//!
//! ## Setting lifecycle
//!
//! ```text
//! App startup   →  Settings::load()   reads settings.json (or returns defaults)
//! UI changed    →  settings.save()    rewrites settings.json on disk
//! Copy started  →  settings.clone()   cheap snapshot sent to the copy thread
//! ```
//!
//! ## JSON serialisation with `serde`
//!
//! The `#[derive(Serialize, Deserialize)]` macros from the `serde` crate
//! automatically generate the code to convert this struct to/from JSON.
//! No manual parsing is required.
//!
//! The `#[serde(default)]` attribute on the struct tells Serde: "if a field is
//! absent from the JSON (e.g. because it was added in a newer version of the app),
//! fill it with the value from `Default::default()` instead of returning an error."
//! This ensures **forward compatibility**: a settings file written by an older
//! version of Bartleby loads cleanly in a newer version.
//!
//! ## Configuration file location
//!
//! The `dirs` crate resolves the platform-appropriate config directory:
//!
//! | OS      | Resolved path                                               |
//! |---------|-------------------------------------------------------------|
//! | Linux   | `~/.config/bartleby/settings.json`                          |
//! | macOS   | `~/Library/Application Support/bartleby/settings.json`      |
//! | Windows | `%APPDATA%\bartleby\settings.json`                          |
//!
//! ## Why JSON and not a database?
//!
//! Bartleby has few preferences that change infrequently. A JSON file is
//! sufficient: it is human-readable, can be edited by hand, and requires no
//! additional dependencies. `serde_json::to_string_pretty` produces indented
//! JSON that is easy to inspect in any text editor.
//!
//! ## Thread safety
//!
//! `Settings` is `Clone + Send`, meaning a snapshot can be cheaply copied and
//! sent to any thread (e.g. the copy engine thread in `copy_engine.rs`). The
//! live instance stored in Tauri's app state is protected by a `Mutex<Settings>`
//! in `main.rs`, which ensures only one thread modifies it at a time.


// ── Main struct ───────────────────────────────────────────────────────────────

/// All user-configurable preferences, serialisable to/from JSON.
///
/// ### Rust derive attributes used
///
/// - `#[derive(Debug)]`
///   Allows the struct to be printed in debug messages:
///   `println!("{:?}", settings)` → `Settings { project_title: "", … }`.
///
/// - `#[derive(Clone)]`
///   `settings.clone()` produces an independent copy.
///   Essential for sending a snapshot to the copy thread without transferring
///   ownership of the original. In Rust, moving a value to a thread prevents
///   using it in the original thread. `clone()` avoids this constraint.
///
/// - `#[derive(serde::Serialize)]`
///   Converts this struct to JSON via `serde_json::to_string()`.
///   Each field becomes a JSON key: `"project_title": "My Film"`.
///
/// - `#[derive(serde::Deserialize)]`
///   Constructs this struct from JSON via `serde_json::from_str()`.
///   Each JSON key is mapped to the corresponding field.
///
/// - `#[serde(default)]`
///   Fields absent from the JSON (written by an older version of the app) are
///   filled with `Default::default()` rather than causing a parse error.
///   This is what makes settings files forward-compatible across app versions.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {

    // ── Report header fields ──────────────────────────────────────────────────
    //
    // These strings are written into the header of the generated CSV and PDF reports.
    // All default to an empty string; if the user leaves them blank, they are
    // simply omitted from the report header without causing any error.

    /// Film or project title. Displayed prominently in the PDF report header.
    /// Example: `"Feature_2024"`, `"Nike_Spot30"`.
    pub project_title: String,

    /// Production company or studio name. Displayed top-right in the PDF.
    /// Example: `"Studio Nord"`, `"Freelance DIT"`.
    pub company: String,

    /// Full name of the person responsible for the data transfer.
    /// Typically the DIT (Digital Imaging Technician) or data manager.
    pub contact_name: String,

    /// Contact email address. Displayed in the PDF report header.
    pub email: String,

    /// Contact phone number. Displayed in the PDF report header.
    pub phone: String,

    /// Absolute path to the company logo image file (JPEG or PNG).
    ///
    /// When non-empty and pointing to a valid image file, the logo is rendered
    /// centred in the PDF report header, between the left column (project title)
    /// and the right column (company / contact info). This gives the report a
    /// professional branded appearance for DIT deliverables.
    ///
    /// Supported formats: JPEG (.jpg / .jpeg) and PNG (.png).
    /// Recommended dimensions: any landscape-ratio image up to ~400×150 px.
    /// The logo is scaled to fit a 40 mm × 15 mm bounding box while preserving
    /// the original aspect ratio ("contain" scaling — no cropping, no distortion).
    ///
    /// Empty string "" (the default) means no logo — the header uses the standard
    /// two-column text layout without a central image.
    pub logo_path: String,

    /// Primary accent colour for PDF reports, stored as a CSS hex string (e.g. "#1F9EDE").
    ///
    /// Used for the column header row background and the page footer header band.
    /// Defaults to the Bartleby blue (#1F9EDE / RGB 0.122, 0.620, 0.871).
    /// Must be a valid 6-digit hex colour with leading "#".
    pub accent_color_1: String,

    /// Secondary accent colour for PDF reports, stored as a CSS hex string (e.g. "#99C7DE").
    ///
    /// Used for the thin decorative horizontal rules above/below the data table
    /// and in the page footer. Defaults to the Bartleby light cyan (#99C7DE).
    /// Must be a valid 6-digit hex colour with leading "#".
    pub accent_color_2: String,

    // ── Active report columns ─────────────────────────────────────────────────
    //
    // Each `bool` controls whether the corresponding column is included in the
    // CSV and PDF report outputs.
    //   `true`  → the column is written to the report
    //   `false` → the column is silently omitted
    //
    // All columns are enabled by default. The user can disable columns that are
    // not relevant to their workflow via the Settings modal (gear button).

    /// File name with extension. Example: `"A001C001_240101.mov"`.
    pub col_name: bool,

    /// File extension in uppercase. Example: `"MP4"`, `"CR3"`, `"WAV"`.
    pub col_type: bool,

    /// Human-readable file size. Example: `"2.34 GB"`, `"450 MB"`, `"128 KB"`.
    pub col_size: bool,

    /// Pixel resolution for images and video. Example: `"1920x1080"`.
    /// Empty for audio files and non-media files.
    pub col_resolution: bool,

    /// Normalised codec name. Example: `"H.264"`, `"ProRes 422 HQ"`, `"FLAC"`.
    pub col_codec: bool,

    /// Playback duration as `HH:MM:SS` or `MM:SS`. Empty for still images.
    pub col_duration: bool,

    /// Bit depth per channel. Example: `"10 bit"`, `"16 bit"`.
    pub col_bit_depth: bool,

    /// Chroma subsampling ratio (video only).
    /// Example: `"4:2:0"`, `"4:2:2"`, `"4:4:4"`.
    pub col_chroma: bool,

    /// Colour space / gamut. Example: `"BT.709"`, `"BT.2020"`, `"sRGB"`.
    pub col_color_space: bool,

    /// Audio sample rate. Example: `"48 kHz"`, `"96 kHz"`.
    pub col_sample_rate: bool,

    /// Full 32-character hexadecimal MD5 digest.
    /// Empty in copy-only mode (no hashing is performed without verify=true).
    pub col_md5: bool,

    // ── Report generation flags ───────────────────────────────────────────────
    //
    // These three booleans correspond to the three checkboxes (.MD5 / .CSV / .PDF)
    // in the options bar. Persisted so the user's preferred combination is restored
    // automatically on the next launch.

    /// Hash algorithm selected for integrity verification and checksum file generation.
    /// Possible values:
    /// - `"none"` — no hashing, copy-only mode (fastest).
    /// - `"md5"`  — MD5 via system crypto library; produces a `.md5` file compatible with `md5sum -c`.
    /// - `"xxh3"` — XXH3-128 via twox-hash; 3×–5× faster than MD5, produces a `.xxh3` file compatible with `xxhsum -c`.
    /// Stored as a string so new algorithms can be added without a breaking schema change.
    pub hash_algo: String,

    /// Generate a `.csv` metadata table report in each destination.
    pub gen_csv: bool,

    /// Generate a `.pdf` visual report with thumbnails and metadata table.
    pub gen_pdf: bool,

    /// Generate a self-contained `.html` report with thumbnails and metadata table.
    pub gen_html: bool,

    /// Open each destination directory in the system file manager after a successful
    /// copy. Handled entirely in JavaScript in the `copy-done` event handler.
    /// Uses `xdg-open` on Linux, `open` on macOS, `explorer` on Windows.
    pub open_dest: bool,

    // ── Appearance ────────────────────────────────────────────────────────────

    /// Light/dark mode selection. Possible values:
    /// - `"default"` — follow the operating system colour scheme.
    ///   On Linux Mint / Cinnamon, detected via the Tauri `is_system_dark_mode`
    ///   command (because the `prefers-color-scheme` CSS media query is unreliable
    ///   inside Tauri's WebKit WebView on Linux).
    /// - `"light"` — always use the light variant of the active skin.
    /// - `"dark"`  — always use the dark variant of the active skin.
    ///
    /// Controls `body.className` in JavaScript:
    ///   "default" → class="theme-default"  (CSS @media (prefers-color-scheme) fallback)
    ///   "light"   → class="theme-light"    (uses :root variables from the skin file)
    ///   "dark"    → class="theme-dark"     (uses body.theme-dark block from the skin file)
    ///
    /// This field is ORTHOGONAL to `skin`: changing the mode does not change
    /// which CSS file is loaded, only which block within that file is active.
    pub theme: String,

    /// Active visual skin — determines which CSS colour palette file is loaded.
    ///
    /// Each skin is a self-contained CSS file at `src/themes/{skin}.css` that
    /// defines all CSS custom properties (--bg, --accent, --radius, etc.).
    ///
    /// JavaScript loads the active skin by swapping the <link id="theme-link"> href:
    ///   `document.getElementById('theme-link').href = 'themes/' + skin + '.css';`
    ///
    /// Possible values (each maps to a file in src/themes/):
    /// - `"mint-y-aqua"` — Linux Mint / Cinnamon (Mint-Y Aqua GTK palette). DEFAULT.
    /// - `"macos"`       — macOS Sequoia / Sonoma (Apple NSColor palette, SF Pro font).
    /// - `"windows11"`   — Windows 11 Fluent Design (WinUI 3 palette, Segoe UI font).
    /// - `"adwaita"`     — GNOME Adwaita / libadwaita (GTK4 palette, Cantarell font).
    ///
    /// ### Orthogonality with `theme`
    /// `skin`  = WHICH colour palette (which CSS file is loaded)
    /// `theme` = light/dark MODE within that palette (which CSS block is active)
    /// Any skin × any mode is valid: 4 skins × 3 modes = 12 possible visual states.
    ///
    /// ### Backward compatibility
    /// This field was added after the initial release. Old settings.json files that
    /// do not contain a "skin" key are handled transparently by `#[serde(default)]`
    /// on the struct: missing fields are filled with Default::default() values
    /// ("mint-y-aqua") rather than causing a parse error. Fully transparent upgrade.
    pub skin: String,
}

// ── Default values ────────────────────────────────────────────────────────────

/// Initial values used in three situations:
///
/// 1. **First launch** — no `settings.json` file exists yet on disk.
/// 2. **Missing fields** — `#[serde(default)]` fills any field absent from an
///    older settings file with the value provided here.
/// 3. **Corrupted file** — if the JSON cannot be parsed, `Settings::load()`
///    falls back to `Settings::default()` and the app starts cleanly.
///
/// ### Design choices
/// - All report columns are `true`: the user sees everything from the start and
///   can disable what they do not need.
/// - Only `.MD5` is enabled by default. `.CSV` and `.PDF` are opt-in because
///   they add processing time and produce extra output files not every workflow needs.
/// - `open_dest = false`: avoids opening unexpected file manager windows on copy completion.
/// - Default theme: `"default"` (follow the OS light/dark setting).
/// - Default skin: `"mint-y-aqua"` (Linux Mint native; neutral on other platforms).
impl Default for Settings {
    fn default() -> Self {
        Self {
            // `String::new()` creates an empty string with no heap allocation.
            // Equivalent to `"".to_string()` but slightly more efficient since
            // no buffer is allocated until the string is actually written to.
            project_title: String::new(),
            company:       String::new(),
            contact_name:  String::new(),
            email:         String::new(),
            phone:         String::new(),
            logo_path:     String::new(), // no logo by default — user must opt in via Settings
            // Default accent colours match the original Bartleby cyan/blue palette.
            // Stored as CSS hex strings so they can be read/written by the JS color picker.
            accent_color_1: "#1F9EDE".to_string(), // Bartleby blue  — column headers
            accent_color_2: "#99C7DE".to_string(), // Bartleby cyan  — decorative rules

            // All metadata columns enabled — show everything by default.
            col_name:        true,
            col_type:        true,
            col_size:        true,
            col_resolution:  true,
            col_codec:       true,
            col_duration:    true,
            col_bit_depth:   true,
            col_chroma:      true,
            col_color_space: true,
            col_sample_rate: true,
            col_md5:         true,

            hash_algo: "md5".to_string(), // MD5 by default — universal and fast enough
            gen_csv:   false,  // opt-in — adds processing time and an extra output file
            gen_pdf:   false,  // opt-in — adds processing time and an extra output file
            gen_html:  false,  // opt-in — produces a self-contained HTML report
            open_dest: false,  // opt-in — avoids unexpected file manager windows

            theme: "default".to_string(), // follow the OS light/dark setting

            // Default skin: Mint-Y Aqua — the native Linux Mint / Cinnamon look.
            // On macOS and Windows, the user will likely change this via the
            // hamburger menu ("Theme" section). A future enhancement could call
            // `get_platform()` at startup and auto-select the appropriate skin.
            skin: "mint-y-aqua".to_string(),
        }
    }
}

// ── Configuration file path ───────────────────────────────────────────────────

/// Returns the absolute path to `settings.json` for the current platform.
///
/// Uses `dirs::config_dir()` from the `dirs` crate, which queries the native OS API:
/// - **Linux**   — `$XDG_CONFIG_HOME` if set, otherwise `~/.config`.
/// - **macOS**   — `NSSearchPathForDirectoriesInDomains` (Foundation framework).
/// - **Windows** — the `%APPDATA%` environment variable.
///
/// The `bartleby` subdirectory is appended to isolate Bartleby's files from
/// other applications sharing the same config directory.
///
/// ### Fallback
/// If `dirs::config_dir()` returns `None` (extremely rare — requires the OS to
/// report no config directory), the path falls back to `"./settings.json"` in
/// the current working directory.
///
/// ### Private visibility
/// This function is `fn` (not `pub fn`): it is only used by `save()` and `load()`
/// within this module. In Rust, the default visibility is private (module-local).
/// Marking it private prevents other modules from accidentally depending on the
/// file path implementation detail.
fn config_path() -> std::path::PathBuf {
    dirs::config_dir()
        // `unwrap_or_else` : if config_dir() returns None, call the closure to
        // produce a fallback value. Preferred over `unwrap_or(PathBuf::from("."))`
        // because the fallback PathBuf is only constructed if actually needed
        // (lazy evaluation), saving a small allocation in the common happy path.
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("bartleby")       // app-specific subdirectory
        .join("settings.json")  // filename
}

// ── Methods ───────────────────────────────────────────────────────────────────

impl Settings {
    /// Serialises the current settings to `settings.json` on disk.
    ///
    /// ### Silent error handling
    /// All I/O errors are silently discarded with `let _ = …`.
    /// A save failure is non-critical: at worst, the user's last changes are not
    /// persisted and will be lost on the next launch. `let _ = expr` is the
    /// idiomatic Rust way to intentionally discard a `Result` — without it the
    /// compiler emits an "unused Result that must be used" warning.
    ///
    /// ### Why `create_dir_all`?
    /// On first launch, `~/.config/bartleby/` does not yet exist.
    /// `create_dir_all` creates all missing path components in one call
    /// (equivalent to `mkdir -p` on the command line). It is idempotent:
    /// calling it on an already-existing directory is a no-op (returns Ok).
    pub fn save(&self) {
        let path = config_path();

        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }

        if let Ok(json) = serde_json::to_string_pretty(self) {
            // Atomic write: write to a sibling .tmp file first, then rename.
            // rename() is atomic on the same filesystem (POSIX guarantee), so a
            // crash or power loss mid-write can never leave settings.json truncated
            // or empty — the old file stays intact until the rename succeeds.
            let tmp = path.with_extension("tmp");
            if let Err(e) = std::fs::write(&tmp, &json) {
                log::warn!("Settings save failed (write temp): {}", e);
                return;
            }
            if let Err(e) = std::fs::rename(&tmp, &path) {
                log::warn!("Settings save failed (rename): {}", e);
                let _ = std::fs::remove_file(&tmp);
            }
        }
    }

    /// Loads settings from `settings.json`, returning `Settings::default()` on
    /// any failure (file absent, unreadable, or containing invalid JSON).
    ///
    /// ### Functional chain explained step by step
    ///
    /// ```text
    /// read_to_string(&path)                 // attempt to read the entire file
    ///   → Ok(String) or Err(io::Error)
    ///
    ///   .ok()                               // discard the error type
    ///   → Some(String) or None (on any I/O error, including file-not-found)
    ///
    ///   .and_then(|s| from_str(&s).ok())    // attempt to parse the JSON string
    ///   → Some(Settings) or None (if JSON is malformed)
    ///
    ///   .unwrap_or_default()                // None → Settings::default()
    ///   → Settings (always a valid, usable value)
    /// ```
    ///
    /// ### Why `.ok()` instead of `?` or `.unwrap()`?
    ///
    /// - `?` would propagate the error to the caller. But `load()` is called at
    ///   app startup, where the only sensible response to a missing or unreadable
    ///   file is to use defaults — not to crash or surface an error to the user.
    /// - `.unwrap()` would panic on first launch (no settings file exists yet).
    ///   That is a bug: first launch must work without any pre-existing config.
    /// - `.ok()` converts `Result<T, E>` to `Option<T>`, silently dropping the
    ///   error variant. Combined with `.unwrap_or_default()`, this provides a
    ///   clean, always-succeeds loading path with no possibility of panic.
    ///
    /// ### Interaction with `#[serde(default)]`
    /// If the JSON is valid but incomplete (e.g. a file from an older Bartleby
    /// version that did not have the `skin` field), `serde_json::from_str` still
    /// succeeds: missing fields are filled with their `Default` values. All other
    /// settings the user had configured are preserved.
    pub fn load() -> Self {
        let path = config_path();

        std::fs::read_to_string(&path)  // read entire file into a String
            .ok()                       // Err (file not found, etc.) → None
            .and_then(|s| {
                // Attempt to deserialise the JSON string into a Settings struct.
                // `.ok()` converts serde_json::Error into None on any parse failure.
                serde_json::from_str(&s).ok()
            })
            .unwrap_or_default()        // None → Settings::default()
    }
}
