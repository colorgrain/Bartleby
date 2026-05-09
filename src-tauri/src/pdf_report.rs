//! # Module `pdf_report`
//!
//! Generates an A4 landscape PDF report with per-file thumbnails and a
//! structured metadata table. Uses the `printpdf 0.7` crate for PDF construction.
//!
//! ## Page layout (A4 landscape: 297 × 210 mm)
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────────┐
//! │  [LEFT COLUMN]                      [RIGHT COLUMN]                      │
//! │  PROJECT TITLE (bold, large)        COMPANY NAME (bold, underlined)     │
//! │  BACKUP REPORT                      Contact Name                        │
//! │  /path/to/source/directory          email@example.com                   │
//! │  Generated  2024-01-15  14:32:00    +33 6 12 34 56 78                  │
//! ├── cyan decorative rule ─────────────────────────────────────────────────┤
//! │  [COLUMN HEADERS — dark blue band, white text]                          │
//! │  Preview  Name  Type  Size  Resolution  Codec  Duration  …  MD5        │
//! ├──────────────────────────────────────────────────────────────────────────┤
//! │  [Row 0 — light grey bg]  [thumb]  clip001.mp4  MP4  2.3 GB  …        │
//! │  [Row 1 — white bg]       [thumb]  clip002.mp4  MP4  1.8 GB  …        │
//! │  …                                                                      │
//! ├── cyan rule ─────────────────────────────────────────────────────────────┤
//! │              Page 1 / 3                Made with Bartleby 0.1 Beta     │
//! └──────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Coordinate system
//!
//! printpdf uses the native PDF coordinate system:
//! - **Y = 0** is at the **bottom** of the page.
//! - **Y increases upward** (opposite to screen coordinates where Y increases down).
//! - The `cursor` variable starts near the **top** (`H - M`) and **decreases** as rows
//!   are added. A row at `cursor` occupies `cursor - RH` to `cursor` vertically.
//!
//! ```text
//! Y = 210 mm (top)  ──► cursor starts here (H - M = 198 mm)
//!                             ↓ cursor decreases per row
//! Y = 0   (bottom) ──► footer drawn here
//! ```
//!
//! ## Thumbnail strategy (priority chain)
//!
//! | File category       | Method                                           |
//! |---------------------|--------------------------------------------------|
//! | Images (JPEG, PNG…) | `image::open` → resize to fit cell               |
//! | RAW camera files    | `image::open` (may fail) → OS icon fallback      |
//! | Video (MP4, MXF…)  | `ffmpeg -ss 1s` extracts one frame as JPEG        |
//! | Audio (WAV, FLAC…) | `ffmpeg showwavespic` renders a waveform PNG      |
//! | Other (PDF, DOCX…) | `python3 + gi` retrieves the OS MIME icon (64 px) |
//! | Final fallback      | Coloured rectangle matching the file type         |
//!
//! ## AUDIT NOTE — printpdf 0.7 API
//! The `add_rect`, `add_shape(Line)`, and `add_polygon` APIs changed between
//! printpdf versions 0.5, 0.6, and 0.7. This module targets exactly 0.7.
//! Upgrading printpdf requires reviewing all drawing calls.
//!
//! ## AUDIT NOTE — temporary files
//! Video and audio thumbnails are written to `std::env::temp_dir()` and deleted
//! immediately after loading. If the process is killed mid-report, up to a few KB
//! of temp files may be left behind. The OS cleans these on reboot or `tmp` purge.

// ── Imports ───────────────────────────────────────────────────────────────────

use std::path::{Path, PathBuf};
// `Path` : borrowed filesystem path (like `&str`). Always used as `&Path`.
// Provides: `.extension()`, `.parent()`, `.join()`, `.display()`, etc.

use std::process::Command;

// ── Windows console suppression ───────────────────────────────────────────────
//
// On Windows, spawning child processes (ffmpeg, python3) creates a visible
// cmd.exe console window that flashes on screen for a fraction of a second.
// `CREATE_NO_WINDOW` (0x08000000) suppresses this via the Windows CreateProcess API.
// The conditional `use` is only compiled on Windows — the trait does not exist
// on Linux/macOS and would cause a compile error if included unconditionally.
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Suppresses the console window that Windows creates when spawning child processes.
///
/// ## Problem
/// On Windows, each `Command::new("ffmpeg")` or `Command::new("python3")` call
/// opens a cmd.exe console window that briefly flashes on screen. During PDF
/// generation with many files, this causes a storm of flickering windows.
///
/// ## Solution
/// The `CREATE_NO_WINDOW` flag (0x08000000) passed to the Windows `CreateProcess`
/// API instructs the OS to start the process without any associated console window.
/// The process runs normally — only the visible window is suppressed.
///
/// ## Platform behaviour
/// - **Windows**: calls `.creation_flags(0x08000000)` via `CommandExt`.
/// - **Linux / macOS**: no-op — Unix processes have no console window concept.
///
/// The function takes `&mut Command` and returns `&mut Command`, so it can be
/// inserted into a builder chain without disrupting the fluent API style.
fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW Win32 API flag
    cmd
}
// `std::process::Command` : builds and spawns external child processes.
// Used to call `ffmpeg` (video frames, audio waveforms) and `python3` (MIME icons).
// `.arg(…)` appends command-line arguments.
// `.output()` runs the process, waits for it, and captures stdout + stderr.
// `.status()` runs the process and returns only the exit code (discards output).
// `.spawn()` starts the process without waiting (non-blocking).

use std::fs::File;
// `std::fs::File` : a handle to an open file.
// `File::create(path)` : creates or truncates. Used for the PDF output file.

use std::io::BufWriter;
// `BufWriter` : wraps a `Write` implementor and buffers writes in userspace.
// printpdf's `doc.save()` makes many small writes internally. BufWriter batches
// them into fewer, larger OS write() syscalls, significantly improving performance.

use chrono::Local;
// `chrono::Local` : the system's local timezone (from the `chrono` crate).
// `Local::now()` : returns the current date and time in local timezone.
// `.format("%Y-%m-%d  %H:%M:%S")` : formats as e.g. "2024-01-15  14:32:00".

use printpdf::*;
// The `printpdf` crate provides PDF construction primitives.
// `use printpdf::*` brings everything into scope: `PdfDocument`, `Mm`, `Px`,
// `PdfPageIndex`, `PdfLayerIndex`, `PdfLayerReference`, `Image`, `ImageXObject`,
// `ImageTransform`, `IndirectFontRef`, `BuiltinFont`, `Color`, `Rgb`,
// `ColorSpace`, `ColorBits`, `Point`, `Polygon`, and more.
// The glob import is acceptable here because printpdf's types are well-namespaced
// and distinct from the rest of the codebase.

use printpdf::path::{PaintMode, WindingOrder};
// These are re-exported under `printpdf::path` in version 0.7.
// `PaintMode::Fill`        : fill the polygon with the current fill colour.
// `WindingOrder::NonZero`  : standard fill rule for simple (non-self-intersecting) polygons.

use ::image::imageops::FilterType;
// `::image` : the `image` crate (the `::` prefix disambiguates from any local `image` module).
// `FilterType::Triangle` : a bilinear resampling filter. Good quality/speed trade-off
// for thumbnail downscaling. Available options: Nearest, Triangle, CatmullRom, Gaussian, Lanczos3.

use ::image::RgbImage;
// `RgbImage` : a heap-allocated 2D array of RGB pixels (no alpha channel).
// Type alias for `ImageBuffer<Rgb<u8>, Vec<u8>>`.
// `.dimensions()` → `(width, height)` in pixels.
// `.into_raw()` → `Vec<u8>` of raw R,G,B bytes (row by row, left to right).
// `.to_rgb8()` : converts any `DynamicImage` to this format (discards alpha).

use crate::metadata::FileMeta;
// `FileMeta` is the struct of technical metadata extracted by `metadata::extract()`.
// Fields: name, file_type, size_human, resolution, codec, duration, etc.
// Defined in `metadata.rs` and `pub` there, so we can use it here.

use crate::settings::Settings;
// `Settings` controls which columns are visible in the report and what text
// appears in the header (project title, company, contact info).

// ── Page geometry (mm) ────────────────────────────────────────────────────────
//
// All layout constants use millimetres, consistent with `printpdf`'s `Mm(…)` unit wrapper.
// ISO 216 A4 paper: 210 × 297 mm (portrait). Landscape = 297 × 210 mm.

/// Page width (A4 landscape wide dimension) = 297 mm.
const W: f32 = 297.0;
/// Page height (A4 landscape narrow dimension) = 210 mm.
const H: f32 = 210.0;
/// Uniform page margin applied to all four sides = 12 mm.
/// The printable area is therefore (W - 2M) × (H - 2M) = 273 × 186 mm.
const M: f32 = 12.0;

// ── Thumbnail cell geometry (mm) ──────────────────────────────────────────────

/// Width of the "Preview" (thumbnail) column = 22 mm.
const TW: f32 = 22.0;
/// Maximum height of a thumbnail image within its cell = 16 mm.
/// The remaining 4 mm of the row height (RH - TH = 4 mm) provides top/bottom padding.
const TH: f32 = 16.0;
/// Total height of one data row = 20 mm.
/// This accommodates the thumbnail (16 mm) + 2 mm padding top and bottom.
const RH: f32 = 20.0;

// ── Font sizes (points) ───────────────────────────────────────────────────────
//
// PDF font sizes are specified in "points" (pt), where 1 pt = 1/72 inch ≈ 0.3528 mm.
// So 7 pt ≈ 2.47 mm character height — readable at A4 scale but compact.
//
// These constants are `f32` because printpdf's `use_text(…, size: f32, …)` expects f32.

/// Font size for the main header elements: project title, company name.
const FS_TITLE: f32 = 14.0; // ≈ 4.9 mm — prominent, first thing the eye sees
/// Font size for sub-heading text: contact info, source path, generated date.
const FS_SUB:   f32 = 7.5;  // ≈ 2.6 mm — legible secondary text
/// Font size for column header labels (Name, Type, Size…).
const FS_HEAD:  f32 = 7.0;  // ≈ 2.5 mm — fits comfortably in the 8 mm header row
/// Font size for data cell text.
const FS_CELL:  f32 = 6.5;  // ≈ 2.3 mm — smallest readable at A4 printing resolution

// ── Colour palette (normalised RGB floats 0.0–1.0) ────────────────────────────
//
// printpdf uses normalised RGB: each channel is an f32 in [0.0, 1.0].
// To convert from 8-bit HTML hex (#RRGGBB): divide each channel by 255.
// Example: #163460 → R=0x16/255=22/255≈0.086, G=0x34/255=52/255≈0.204, B=0x60/255=96/255≈0.373
//
// Colours are tuples `(f32, f32, f32)` = (Red, Green, Blue).
// Accessed as `COLOR.0`, `COLOR.1`, `COLOR.2` in function calls.

// Default accent colour — used as fallback when settings hex is invalid.
const DEFAULT_ACCENT1: (f32, f32, f32) = (0.122, 0.620, 0.871); // #1F9EDE Bartleby blue
/// Light grey (#F1F3F6) — alternating row background for even-numbered rows.
const ROW_EVEN:   (f32, f32, f32) = (0.945, 0.953, 0.965);
/// Pure white (#FFFFFF) — alternating row background for odd-numbered rows.
const ROW_ODD:    (f32, f32, f32) = (1.000, 1.000, 1.000);
/// Near-black (#222222) — primary text colour on white/light backgrounds.
const TEXT_DARK:  (f32, f32, f32) = (0.133, 0.133, 0.133);
/// Pure white (#FFFFFF) — text colour on dark backgrounds (column headers).
const TEXT_WHITE: (f32, f32, f32) = (1.000, 1.000, 1.000);
/// Medium grey (#666672) — secondary text: contact info, source path, generated date.
const TEXT_MID:   (f32, f32, f32) = (0.400, 0.420, 0.450);

// ── File type classification (for thumbnail selection) ────────────────────────
//
// These lists determine which thumbnail strategy is applied per file.
// They parallel the lists in `metadata.rs` but are kept separate to avoid coupling:
// `pdf_report.rs` decides *how to render* a file; `metadata.rs` decides *what to query*.
//
// Type `&[&str]` : a slice of string literal references.
// - `&str` : a borrowed string slice. String literals ("jpg") have type `&'static str`
//   (they live in the binary's read-only data section for the entire program lifetime).
// - `&[…]` : a slice — a pointer to the first element + a count of elements.
// No heap allocation occurs; these slices point directly into the binary.

/// Still image formats directly decodable by the `image` crate.
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg",       // JPEG — most common camera format
    "png",               // Portable Network Graphics
    "tiff", "tif",       // TIFF — common in professional photography
    "webp",              // Google WebP
    "bmp",               // Windows Bitmap
    "gif",               // Graphics Interchange Format
    "heic", "heif",      // Apple HEIF/HEIC (iPhone 12+, modern mirrorless)
    "cr2", "cr3",        // Canon RAW (CR3 since EOS R series)
    "nef",               // Nikon Electronic Format
    "arw",               // Sony Alpha RAW
    "dng",               // Adobe Digital Negative (open RAW container)
    // Note: most RAW formats below (cr2, cr3, nef, arw, dng) will fail in
    // `image::open()` and fall through to the OS MIME icon. Only JPEG-based
    // RAW previews (some DNG files) may succeed.
];

/// Video formats for which `ffmpeg` frame extraction is attempted.
const VIDEO_EXTS: &[&str] = &[
    "mp4", "mov",        // Consumer/prosumer containers (H.264, H.265, ProRes)
    "mxf",               // Material eXchange Format (broadcast, cinema, Sony XDCAM)
    "avi",               // Audio Video Interleave (legacy Windows)
    "mkv",               // Matroska (open container, common online)
    "m4v",               // iTunes video
    "wmv",               // Windows Media Video
    "flv",               // Flash Video (legacy web)
    "webm",              // WebM (VP8/VP9 for browsers)
    "m2ts", "mts", "ts", // MPEG-2 Transport Stream (Blu-ray, Sony cameras)
    "r3d",               // RED Cinema proprietary RAW video
    "braw",              // Blackmagic RAW (BMPCC cameras)
    "mpg", "mpeg",       // MPEG-1/2 video (legacy)
];

/// Audio-only formats for which `ffmpeg showwavespic` is attempted.
const AUDIO_EXTS: &[&str] = &[
    "mp3",               // MPEG Audio Layer III
    "wav",               // Waveform Audio (uncompressed PCM)
    "aac",               // Advanced Audio Coding
    "flac",              // Free Lossless Audio Codec
    "ogg",               // Ogg Vorbis
    "m4a",               // MPEG-4 Audio (AAC in MP4 container)
    "aif", "aiff",       // Audio Interchange File Format (Apple/pro audio)
    "opus",              // Opus (low latency, VoIP)
    "wma",               // Windows Media Audio
    "alac",              // Apple Lossless Audio Codec
];

// ── Public entry point ────────────────────────────────────────────────────────

/// Generates a PDF report and writes it to `{dst_dir}/{src_name}_report.pdf`.
///
/// # Parameters
/// - `dst_dir`  : destination directory. The PDF is written inside it.
/// - `src_name` : base name for the output file and the report's internal title.
///                Example: `"SHOOT_2024"` → file `SHOOT_2024_report.pdf`.
/// - `src_path` : absolute path to the source directory. Used to resolve the
///                absolute path of each file for thumbnail extraction (by joining
///                `src_path` with the relative path `rel` from each entry).
/// - `entries`  : slice of 4-tuples `(FileMeta, md5_hash, rel_path, verify_ok)`:
///   - `FileMeta`     : technical metadata (name, size, codec, etc.)
///   - `String`       : 32-char MD5 hex hash (or `""` in copy-only mode)
///   - `String`       : file path relative to the source root (e.g. `"PRIVATE/A001.MXF"`)
///   - `Option<bool>` : `Some(true)` = verified OK, `Some(false)` = mismatch, `None` = not verified
/// - `settings` : column visibility flags and report header content (project title, company…)
///
/// # Page management
/// A new page is added automatically when `cursor < M + RH + 2.0`.
/// The column header row is redrawn at the top of each new page so every page
/// is self-contained (readable without referring back to page 1).
///
/// # Returns
/// `std::io::Result<()>` — `Ok(())` on success, `Err(e)` if any I/O fails
/// (e.g. permission denied writing the output file, or disk full).
/// The `?` operator inside propagates errors to the caller (`copy_engine::run`).


/// One styled word parsed from the WYSIWYG comment HTML.
#[derive(Clone)]
struct RichWord {
    text:   String,
    bold:   bool,
    italic: bool,  // includes <u> — renders as italic in PDF (no native underline in built-in fonts)
}

/// Parses HTML comment into a flat sequence of styled words and newline tokens.
///
/// Supports `<b>/<strong>`, `<i>/<em>/<u>`, `<br>`, and closing `</div>`/`</p>`.
/// All other tags are stripped. HTML entities are decoded.
fn parse_html_to_rich_words(html: &str) -> Vec<RichWord> {
    let mut out: Vec<RichWord> = Vec::new();
    let mut bold:   u32 = 0;
    let mut italic: u32 = 0;
    let mut buf  = String::new();
    let mut chars = html.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '<' { buf.push(ch); continue; }
        // Flush buffered plain text as words
        for word in buf.split_whitespace() {
            out.push(RichWord { text: word.to_string(), bold: bold > 0, italic: italic > 0 });
        }
        buf.clear();
        // Parse tag
        let mut closing = false;
        if chars.peek() == Some(&'/') { chars.next(); closing = true; }
        let mut tag = String::new();
        while let Some(&c) = chars.peek() {
            if c == '>' || c == ' ' || c == '\t' || c == '/' { break; }
            tag.push(c); chars.next();
        }
        while let Some(c) = chars.next() { if c == '>' { break; } }
        let tag_lc = tag.to_lowercase();
        match tag_lc.as_str() {
            "b" | "strong" => {
                if closing { bold   = bold.saturating_sub(1);   }
                else       { bold   += 1; }
            }
            "i" | "em" | "u" => {
                if closing { italic = italic.saturating_sub(1); }
                else       { italic += 1; }
            }
            "br" | "div" | "p" | "li" =>
                out.push(RichWord { text: "\n".into(), bold: false, italic: false }),
            _ => {}
        }
    }
    for word in buf.split_whitespace() {
        out.push(RichWord { text: word.to_string(), bold: bold > 0, italic: italic > 0 });
    }
    // Decode HTML entities
    for w in &mut out {
        w.text = w.text
            .replace("&amp;",  "&")
            .replace("&lt;",   "<")
            .replace("&gt;",   ">")
            .replace("&nbsp;", " ")
            .replace("&#39;",  "'")
            .replace("&quot;", "\"");
    }
    out
}

/// Reflows styled words into lines that fit within `max_mm` at font size `fs`.
fn wrap_rich_words(words: &[RichWord], max_mm: f32, fs: f32) -> Vec<Vec<RichWord>> {
    let space_w = text_width_mm(" ", fs, false);

    let mut lines: Vec<Vec<RichWord>> = Vec::new();
    let mut cur:   Vec<RichWord>      = Vec::new();
    let mut cur_w  = 0.0f32;

    for word in words {
        if word.text == "\n" {
            if !cur.is_empty() { lines.push(std::mem::take(&mut cur)); cur_w = 0.0; }
            continue;
        }
        let ww       = text_width_mm(&word.text, fs, word.bold);
        let needed_w = if cur.is_empty() { ww } else { space_w + ww };
        if cur_w + needed_w > max_mm && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            cur_w = 0.0;
        }
        cur_w += if cur.is_empty() { ww } else { space_w + ww };
        cur.push(word.clone());
    }
    if !cur.is_empty() { lines.push(cur); }
    lines
}

/// Draws the rich-text comment block with a "Comments:" label and advances `cursor`.
///
/// Renders "Comments:" bold on its own line, then the comment text below.
fn draw_rich_comment(
    layer:     &PdfLayerReference,
    font_reg:  &IndirectFontRef,
    font_bold: &IndirectFontRef,
    font_ital: &IndirectFontRef,
    font_bi:   &IndirectFontRef,
    comment:   &str,
    cursor:    &mut f32,
) {
    const FS:       f32 = 7.5;
    const LABEL_FS: f32 = 8.0;
    const LINE_H:   f32 = 4.5;
    const TOP_PAD:  f32 = 2.5;
    const BOT_PAD:  f32 = 3.0;
    const TEXT_X:   f32 = M + 2.0;

    let max_mm  = W - M - TEXT_X;
    let space_w = text_width_mm(" ", FS, false);

    let words = parse_html_to_rich_words(comment);
    if !words.iter().any(|w| w.text != "\n" && !w.text.trim().is_empty()) { return; }

    let lines = wrap_rich_words(&words, max_mm, FS);
    if lines.is_empty() { return; }

    // +1 line for the "Comments:" label
    let total_h = TOP_PAD + LINE_H + lines.len() as f32 * LINE_H + BOT_PAD;
    if *cursor - total_h < M + 12.0 { return; }

    *cursor -= TOP_PAD;

    // "Comments:" label — bold, dark, underlined
    let label_ty = *cursor - 3.2;
    set_color(layer, TEXT_DARK);
    layer.use_text("Comments:", LABEL_FS, Mm(TEXT_X), Mm(label_ty), font_bold);
    let label_w = text_width_mm("Comments:", LABEL_FS, true);
    fill_rect(layer, TEXT_X, label_ty - 1.0, TEXT_X + label_w, label_ty - 0.5, 0.0, 0.0, 0.0);
    *cursor -= LINE_H;

    // Comment text lines
    for line in &lines {
        let ty = *cursor - 3.2;
        set_color(layer, TEXT_DARK);
        let mut tx    = TEXT_X;
        let mut first = true;
        for word in line {
            if !first { tx += space_w; }
            first = false;
            let font = match (word.bold, word.italic) {
                (true,  true)  => font_bi,
                (true,  false) => font_bold,
                (false, true)  => font_ital,
                (false, false) => font_reg,
            };
            layer.use_text(&word.text, FS, Mm(tx), Mm(ty), font);
            tx += text_width_mm(&word.text, FS, word.bold);
        }
        *cursor -= LINE_H;
    }
    *cursor -= BOT_PAD;
}

// Wrapper that calls parse_hex_color with the ? operator inside an Option closure.
fn hex_to_rgb(hex: &str, default: (f32, f32, f32)) -> (f32, f32, f32) {
    (|| -> Option<(f32, f32, f32)> {
        let hex = hex.trim_start_matches('#');
        if hex.len() != 6 { return None; }
        let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
        let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
        let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
        Some((r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0))
    })().unwrap_or(default)
}

pub fn write_pdf(
    dst_dir:         &Path,
    src_name:        &str,
    src_path:        &Path,
    src_total_bytes: u64,
    destinations:    &[PathBuf],
    entries:         &[(FileMeta, String, String, String, Option<bool>)],
    settings:        &Settings,
    gen_md5:         bool,
    gen_xxh:         bool,
    comment:         &str,
) -> std::io::Result<()> {
    let accent1 = hex_to_rgb(&settings.accent_color_1, DEFAULT_ACCENT1);

    // ── Timestamp ──────────────────────────────────────────────────────────────
    // `Local::now()` : current date/time in the system's local timezone.
    // `.format(…)` : applies a strftime-style format string (from chrono).
    //   `%Y` = 4-digit year, `%m` = 2-digit month, `%d` = day, etc.
    // `.to_string()` : converts chrono's `DelayedFormat` to an owned `String`.
    let now = Local::now().format("%Y-%m-%d at %I:%M %p").to_string();

    // ── Create PDF document ────────────────────────────────────────────────────
    // `PdfDocument::new(title, width, height, first_layer_name)` creates a new
    // document and returns a 3-tuple:
    //   - `PdfDocumentReference` : the document handle (cloneable, Arc-backed).
    //   - `PdfPageIndex`         : index of the first page.
    //   - `PdfLayerIndex`        : index of the first layer on the first page.
    //
    // In PDF, every page has at least one "layer" (called a "content stream" in the
    // PDF specification). We draw everything on the default layer.
    //
    // `Mm(W)` : wraps the width in printpdf's `Mm` unit type.
    // printpdf uses newtype wrappers (Mm, Pt, Px) to prevent accidental unit mixing.
    let (doc, p1, l1) = PdfDocument::new(
        format!("Bartleby — {}", src_name),  // PDF document title (visible in Acrobat tab)
        Mm(W), Mm(H),                         // A4 landscape: 297 × 210 mm
        "Page 1",                             // name of the first content layer
    );

    // ── Load built-in fonts ────────────────────────────────────────────────────
    // PDF has 14 standard "built-in" fonts that every conforming PDF viewer must
    // provide without embedding. Using them keeps the file size small (no font data).
    //
    // `doc.add_builtin_font(BuiltinFont::Helvetica)` returns `IndirectFontRef`,
    // which is printpdf's handle to a font resource. It is used in `layer.use_text(…)`.
    //
    // `.unwrap()` is safe here: built-in fonts always load successfully. A failure
    // would indicate a bug in the printpdf library itself.
    let font_reg     = doc.add_builtin_font(BuiltinFont::Helvetica).unwrap();
    let font_bold    = doc.add_builtin_font(BuiltinFont::HelveticaBold).unwrap();
    let font_ital    = doc.add_builtin_font(BuiltinFont::HelveticaOblique).unwrap();
    let font_bold_it = doc.add_builtin_font(BuiltinFont::HelveticaBoldOblique).unwrap();

    // ── Page index ─────────────────────────────────────────────────────────────
    // `pages` accumulates (PdfPageIndex, PdfLayerIndex) pairs as new pages are added.
    // We keep all page indices so we can draw footers on every page at the end,
    // once we know the total page count. Footers say "Page N / M" — M is unknown
    // until all data rows have been placed.
    let mut pages: Vec<(PdfPageIndex, PdfLayerIndex)> = vec![(p1, l1)];

    // ── Vertical cursor ────────────────────────────────────────────────────────
    // Set by draw_header's return value on page 1, reset to H-M on each new page.
    // IMPORTANT: In printpdf, Y=0 is at the BOTTOM of the page.
    let mut cursor: f32;

    // ── Status column detection ────────────────────────────────────────────────
    // If at least one entry has a non-None `verify_ok`, we add a "St." status column.
    // `.iter().any(|item| predicate)` : returns `true` if predicate is true for any item.
    //   Short-circuits on the first `true` — does not process all entries unnecessarily.
    // The closure destructures the 4-tuple: `(_, _, _, ok)` — the `_` discards fields
    // we don't need. `ok.is_some()` checks if verify_ok is Some(true) or Some(false).
    // The tuple is now (FileMeta, md5, xxh3, rel, ok) — 5 fields.
    let has_status = entries.iter().any(|(_, _, _, _, ok)| ok.is_some());

    // ── Draw page 1 header ─────────────────────────────────────────────────────
    // A block scope `{ … }` creates a lexical scope for `layer`. This ensures `layer`
    // (which borrows `doc`) is dropped before the data row loop below, avoiding a
    // borrow conflict: Rust prevents using `doc` (to add pages) while `layer` still
    // holds a borrow on it.
    {
        // `doc.get_page(index)` → `PdfPageReference` (borrows `doc`).
        // `.get_layer(index)` → `PdfLayerReference` (borrows the page).
        // Drawing operations are called on the `PdfLayerReference`.
        let layer   = doc.get_page(pages[0].0).get_layer(pages[0].1);
        let rule_y  = draw_header(&layer, &font_bold, &font_reg, src_path, src_total_bytes, &now, settings, &destinations);
        cursor      = rule_y;
        draw_rich_comment(&layer, &font_reg, &font_bold, &font_ital, &font_bold_it, comment, &mut cursor);
        cursor -= 2.0;
        draw_col_headers(&layer, &font_bold, settings, &mut cursor, has_status, gen_md5, gen_xxh, accent1);
    } // `layer` is dropped here, releasing the borrow on `doc`

    // ════════════════════════════════════════════════════════════════════════════
    // DATA ROWS — sorted by directory then filename, with directory separator rows
    // ════════════════════════════════════════════════════════════════════════════

    // Build a sorted index: entries are ordered by (directory, filename), both
    // lower-cased so the sort is case-insensitive.
    let mut sorted_indices: Vec<usize> = (0..entries.len()).collect();
    sorted_indices.sort_by(|&a, &b| {
        sort_key_for_rel(&entries[a].3).cmp(&sort_key_for_rel(&entries[b].3))
    });

    let mut current_dir: Option<String> = None; // tracks the last-drawn directory
    let mut file_row_i:  usize = 0;             // counts file rows for alternating bg

    for &entry_idx in &sorted_indices {
        let (meta, md5, xxh3, rel, verify_ok) = &entries[entry_idx];

        // Compute the directory component of this entry's relative path.
        let dir = {
            let p = Path::new(rel.as_str());
            p.parent()
                .map(|parent| parent.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default()
        };

        // ── Directory separator row ────────────────────────────────────────────
        // Drawn once per directory, before the first file in that directory.
        if current_dir.as_deref() != Some(dir.as_str()) {
            current_dir = Some(dir.clone());

            if cursor < M + RH + 2.0 {
                let (np, nl) = doc.add_page(Mm(W), Mm(H), format!("Page {}", pages.len() + 1));
                pages.push((np, nl));
                cursor = H - M;
                let layer = doc.get_page(np).get_layer(nl);
                draw_col_headers(&layer, &font_bold, settings, &mut cursor, has_status, gen_md5, gen_xxh, accent1);
            }
            {
                let (pi, li) = *pages.last().unwrap();
                let layer = doc.get_page(pi).get_layer(li);
                let dir_label = if dir.is_empty() { "/".to_string() } else { dir.clone() };
                draw_dir_separator(&layer, &font_bold, &dir_label, cursor, accent1);
            }
            cursor -= RH;
        }

        // ── Page break check ───────────────────────────────────────────────────
        if cursor < M + RH + 2.0 {
            let (np, nl) = doc.add_page(Mm(W), Mm(H), format!("Page {}", pages.len() + 1));
            pages.push((np, nl));
            cursor = H - M;
            let layer = doc.get_page(np).get_layer(nl);
            draw_col_headers(&layer, &font_bold, settings, &mut cursor, has_status, gen_md5, gen_xxh, accent1);
        }

        // ── Get the layer for the current (last) page ──────────────────────────
        let (pi, li) = *pages.last().unwrap();
        let layer    = doc.get_page(pi).get_layer(li);

        // ── Alternating row background (counts only file rows, not dir rows) ───
        let bg = if file_row_i % 2 == 0 { ROW_EVEN } else { ROW_ODD };
        fill_rect(&layer, M, cursor - RH, W - M, cursor, bg.0, bg.1, bg.2);

        // ── Row separator ──────────────────────────────────────────────────────
        draw_hline(&layer, M, W - M, cursor - RH);

        // ── Thumbnail ──────────────────────────────────────────────────────────
        let rel_native = rel.replace('/', std::path::MAIN_SEPARATOR_STR);
        let file_path  = src_path.join(&rel_native);

        let ext = Path::new(&meta.name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();

        draw_thumb(&doc, &layer, &file_path, &ext, cursor, bg);

        // ── Vertical position for text in this row ─────────────────────────────
        set_color(&layer, TEXT_DARK);
        let ty     = cursor - RH / 2.0 - 1.5;
        let mut tx = M + TW + 2.0;

        // ── Status column (OK / ERR) ───────────────────────────────────────────
        let has_status = verify_ok.is_some();
        if has_status {
            let label = match verify_ok {
                Some(true)  => "OK",
                Some(false) => "ERR",
                None        => "",
            };
            let (r, g, b) = match verify_ok {
                Some(true)  => (0.15_f32, 0.55_f32, 0.20_f32),
                Some(false) => (0.75_f32, 0.10_f32, 0.10_f32),
                None        => (0.5_f32,  0.5_f32,  0.5_f32),
            };
            layer.set_fill_color(Color::Rgb(Rgb::new(r, g, b, None)));
            layer.use_text(label, FS_CELL + 1.0, Mm(tx + 1.0), Mm(ty), &font_reg);
            set_color(&layer, TEXT_DARK);
            tx += 10.0;
        }

        // ── Data columns ───────────────────────────────────────────────────────
        let cols   = active_cols(meta, md5, xxh3, settings, gen_md5, gen_xxh);
        let widths = col_widths(settings, gen_md5, gen_xxh);

        for (idx, (val, w)) in cols.iter().zip(widths.iter()).enumerate() {
            if idx == 0 && settings.col_name {
                let (line1, line2) = wrap_text(val, *w - 1.0, FS_CELL);
                if line2.is_some() {
                    layer.use_text(&line1, FS_CELL, Mm(tx), Mm(ty + 2.2), &font_reg);
                    layer.use_text(
                        line2.as_deref().unwrap_or(""),
                        FS_CELL, Mm(tx), Mm(ty - 1.5), &font_reg,
                    );
                } else {
                    layer.use_text(&line1, FS_CELL, Mm(tx), Mm(ty), &font_reg);
                }
            } else if val.contains('\n') {
                let mut parts = val.splitn(2, '\n');
                let line1 = parts.next().unwrap_or("");
                let line2 = parts.next().unwrap_or("");
                layer.use_text(clip(line1, *w, FS_CELL - 0.5), FS_CELL - 0.5, Mm(tx), Mm(ty + 2.2), &font_reg);
                layer.use_text(clip(line2, *w, FS_CELL - 0.5), FS_CELL - 0.5, Mm(tx), Mm(ty - 1.5), &font_reg);
            } else {
                layer.use_text(clip(val, *w, FS_CELL), FS_CELL, Mm(tx), Mm(ty), &font_reg);
            }
            tx += w;
        }

        cursor -= RH;
        file_row_i += 1;
    }

    // ════════════════════════════════════════════════════════════════════════════
    // FOOTERS — one per page, drawn after all rows are placed
    // ════════════════════════════════════════════════════════════════════════════
    //
    // Footers contain "Page N / M" where M = total page count. We only know M
    // after all data rows have been placed (adding a row may start a new page).
    // So we draw footers in a second pass over all pages.
    //
    // `pages.iter().enumerate()` yields `(0-based_index, &(PdfPageIndex, PdfLayerIndex))`.
    // `page_i + 1` converts to 1-based page numbers for display.
    for (page_i, (pi, li)) in pages.iter().enumerate() {
        let layer = doc.get_page(*pi).get_layer(*li);
        // `*pi` and `*li` : dereference the references from `pages.iter()`.
        draw_footer(&layer, &font_reg, page_i + 1, pages.len(), entries.len(), accent1);
    }

    // ════════════════════════════════════════════════════════════════════════════
    // SAVE — write the PDF to disk
    // ════════════════════════════════════════════════════════════════════════════
    //
    // `dst_dir.join(…)` : appends the filename to the destination directory path.
    // `File::create(path)?` : creates (or overwrites) the file. The `?` operator
    //   propagates the `io::Error` to the caller if the file cannot be created.
    // `BufWriter::new(file)` : buffers writes. printpdf's `doc.save()` internally
    //   calls `write()` many times; without BufWriter each call would be a syscall.
    // `doc.save(&mut wr)` : serialises the PDF document to the writer.
    //   Returns `Result<(), printpdf::Error>`.
    // `.map_err(|e| std::io::Error::new(…, e.to_string()))` :
    //   converts `printpdf::Error` to `std::io::Error` so the return type is
    //   consistent: `std::io::Result<()>` throughout this file.
    let path   = dst_dir.join(format!("{}_report.pdf", src_name));
    let mut wr = BufWriter::new(File::create(path)?);
    doc.save(&mut wr)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
    Ok(()) // Signal success to the caller (`copy_engine::run`)
}

// ── Header drawing ────────────────────────────────────────────────────────────

/// Draws the report header and returns the Y position where the rule should be drawn.
///
/// ## Layout
///
/// ```text
/// ┌──────────────────────────────────────────────────────────────────────────────┐
/// │ [Logo]  (top-left, optional, max 30×12 mm)                                  │
/// │ COMPANY NAME  (bold, left-aligned)                                           │
/// │ Contact / Email / Tel  (grey, one line, left-aligned)                        │
/// │                                                                              │
/// │                    PROJECT NAME  (bold, underlined, centered)                │
/// │               Backup Report – YYYY-MM-DD at HH:MM:SS  (bold, centered)      │
/// │              Source : /path/… – 2.34 GB  (grey, centered)                   │
/// │                  Destination 1 : /path/…  (grey, centered)                  │
/// └──────────────────────────────────────────────────────────────────────────────┘
/// ```
fn draw_header(
    layer:           &PdfLayerReference,
    bold:            &IndirectFontRef,
    reg:             &IndirectFontRef,
    src_path:        &Path,
    src_total_bytes: u64,
    now:             &str,
    settings:        &Settings,
    destinations:    &[PathBuf],
) -> f32 {
    let top_y = H - M - 4.0;
    let mut ly = top_y;

    // ── LEFT BLOCK — Logo + Company + Contact (drawn first) ──────────────────

    if !settings.logo_path.is_empty() {
        if let Ok(dyn_img) = ::image::open(&settings.logo_path) {
            let rgb = rgba_to_rgb_white(dyn_img.into_rgba8());
            let (iw, ih) = (rgb.width() as f32, rgb.height() as f32);
            const LW: f32 = 30.0;
            const LH: f32 = 12.0;
            let scale = (LW / iw).min(LH / ih).min(1.0);
            let dw = iw * scale;
            let dh = ih * scale;
            let dpi: f32 = 96.0;
            let px_per_mm = dpi / 25.4;
            let xobj = ImageXObject {
                width:              Px(iw as usize),
                height:             Px(ih as usize),
                color_space:        ColorSpace::Rgb,
                bits_per_component: ColorBits::Bit8,
                interpolate:        true,
                image_data:         rgb.into_raw(),
                image_filter:       None,
                smask:              None,
                clipping_bbox:      None,
            };
            Image::from(xobj).add_to_layer(layer.clone(), ImageTransform {
                translate_x: Some(Mm(M)),
                translate_y: Some(Mm(ly - dh)),
                scale_x:     Some(dw / (iw / px_per_mm)),
                scale_y:     Some(dh / (ih / px_per_mm)),
                rotate:      None,
                dpi:         Some(dpi),
            });
            ly -= dh + 7.0;
        }
    }

    if !settings.company.is_empty() {
        set_color(layer, TEXT_DARK);
        layer.use_text(settings.company.to_uppercase(), FS_TITLE, Mm(M), Mm(ly), bold);
        ly -= 5.0;
    }

    let contact_line: String = [
        settings.contact_name.as_str(),
        settings.email.as_str(),
        settings.phone.as_str(),
    ]
    .iter()
    .filter(|s| !s.is_empty())
    .cloned()
    .collect::<Vec<_>>()
    .join(" / ");

    if !contact_line.is_empty() {
        set_color(layer, TEXT_MID);
        layer.use_text(&contact_line, FS_SUB, Mm(M), Mm(ly), reg);
        ly -= 4.5;
    }

    // ── CENTER BLOCK — Project name + report info (starts below left block) ──

    // 5 mm gap between contact line and project title.
    let mut cy = ly - 5.0;

    // Max chars that fit in the full printable width (used for path truncation).
    let max_line_chars = ((W - 2.0 * M - 4.0) / (FS_SUB * 0.155)) as usize;

    // PROJECT NAME — centered, bold, underlined
    if !settings.project_title.is_empty() {
        let project_text = settings.project_title.to_uppercase();
        set_color(layer, (0.0, 0.0, 0.0));
        let text_w = text_width_mm(&project_text, FS_TITLE + 2.0, true);
        let cx = ((W / 2.0) - text_w / 2.0).max(M);
        layer.use_text(&project_text, FS_TITLE + 2.0, Mm(cx), Mm(cy), bold);
        // Manual underline: thin filled rect 0.8 mm below baseline
        fill_rect(layer, cx, cy - 1.3, (cx + text_w).min(W - M), cy - 0.7, 0.0, 0.0, 0.0);
        cy -= 7.0;
    }

    // "Backup Report – YYYY-MM-DD at HH:MM:SS" — bold, centered
    let report_line = format!("Backup Report  –  {}", now);
    set_color(layer, TEXT_DARK);
    let rw = text_width_mm(&report_line, FS_TITLE - 3.0, true);
    let rx = ((W / 2.0) - rw / 2.0).max(M);
    layer.use_text(&report_line, FS_TITLE - 3.0, Mm(rx), Mm(cy), bold);
    cy -= 5.5;

    // "Source : /path – size" — regular, centered
    let src_str  = src_path.to_string_lossy().to_string();
    let size_str = crate::metadata::format_size(src_total_bytes);
    let overhead = format!("Source :   –  {}", size_str).chars().count();
    let max_path = max_line_chars.saturating_sub(overhead);
    let src_disp = truncate_path_tail(&src_str, max_path);
    let src_line = format!("Source : {}  –  {}", src_disp, size_str);
    set_color(layer, TEXT_MID);
    let sw = text_width_mm(&src_line, FS_SUB, false);
    let sx = ((W / 2.0) - sw / 2.0).max(M);
    layer.use_text(&src_line, FS_SUB, Mm(sx), Mm(cy), reg);
    cy -= 4.5;

    // Destinations — regular, centered, slightly smaller
    let max_dst = max_line_chars.saturating_sub(20); // subtract "Destination N : " overhead
    for (i, dst) in destinations.iter().enumerate() {
        let dst_str  = dst.to_string_lossy().to_string();
        let dst_disp = truncate_path_tail(&dst_str, max_dst);
        let dst_line = format!("Destination {} : {}", i + 1, dst_disp);
        let dw = text_width_mm(&dst_line, FS_SUB - 0.5, false);
        let dx = ((W / 2.0) - dw / 2.0).max(M);
        layer.use_text(&dst_line, FS_SUB - 0.5, Mm(dx), Mm(cy), reg);
        cy -= 4.0;
    }

    cy - 3.0
}

/// Returns the rendered width in mm for a string in Helvetica (or Helvetica-Bold)
/// using the official Adobe AFM glyph-width table (units/1000 em).
/// Fallback for non-ASCII characters: 600 units (reasonable Latin average).
fn text_width_mm(text: &str, font_size_pt: f32, bold: bool) -> f32 {
    // AFM widths for codepoints 32–126 (space → tilde), Helvetica regular then bold.
    #[rustfmt::skip]
    const REG: [u16; 95] = [
    //  sp   !    "    #    $    %    &    '    (    )    *    +    ,    -    .    /
        278, 278, 355, 556, 556, 889, 667, 222, 333, 333, 389, 584, 278, 333, 278, 278,
    //  0    1    2    3    4    5    6    7    8    9
        556, 556, 556, 556, 556, 556, 556, 556, 556, 556,
    //  :    ;    <    =    >    ?    @
        278, 278, 584, 584, 584, 556,1015,
    //  A    B    C    D    E    F    G    H    I    J    K    L    M    N    O    P    Q    R    S    T    U    V    W    X    Y    Z
        667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611,
    //  [    \    ]    ^    _    `
        278, 278, 278, 469, 556, 222,
    //  a    b    c    d    e    f    g    h    i    j    k    l    m    n    o    p    q    r    s    t    u    v    w    x    y    z
        556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500,
    //  {    |    }    ~
        334, 260, 334, 584,
    ];
    #[rustfmt::skip]
    const BOLD: [u16; 95] = [
    //  sp   !    "    #    $    %    &    '    (    )    *    +    ,    -    .    /
        278, 333, 474, 556, 556, 889, 722, 278, 333, 333, 389, 584, 278, 333, 278, 278,
    //  0    1    2    3    4    5    6    7    8    9
        556, 556, 556, 556, 556, 556, 556, 556, 556, 556,
    //  :    ;    <    =    >    ?    @
        333, 333, 584, 584, 584, 611, 975,
    //  A    B    C    D    E    F    G    H    I    J    K    L    M    N    O    P    Q    R    S    T    U    V    W    X    Y    Z
        722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778, 667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611,
    //  [    \    ]    ^    _    `
        333, 278, 333, 584, 556, 278,
    //  a    b    c    d    e    f    g    h    i    j    k    l    m    n    o    p    q    r    s    t    u    v    w    x    y    z
        611, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611, 611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500,
    //  {    |    }    ~
        389, 280, 389, 584,
    ];
    let table = if bold { &BOLD } else { &REG };
    let units: f32 = text.chars().map(|c| {
        let cp = c as u32;
        if cp >= 32 && cp <= 126 { table[(cp - 32) as usize] as f32 } else { 600.0 }
    }).sum();
    units / 1000.0 * font_size_pt * 0.3528
}

/// Truncates a path string to `max_chars` by keeping the tail, prefixed with "…".
fn truncate_path_tail(path: &str, max_chars: usize) -> String {
    if path.chars().count() <= max_chars { return path.to_string(); }
    let tail_start = path.len().saturating_sub(
        path.char_indices().nth(max_chars)
            .map(|(i, _)| i)
            .unwrap_or(path.len()),
    );
    format!("…{}", &path[tail_start..])
}

/// Draws the dark blue column header band with white labels, and advances `cursor`.
///
/// Called once per page: on page 1 from `write_pdf`, and at the top of each
/// subsequent page after a page break.
///
/// ### `cursor: &mut f32` — mutable reference parameter
/// This is one of Rust's most important concepts for Rust learners:
/// - `&f32` : shared (immutable) reference — "I borrow this, I won't change it".
/// - `&mut f32` : exclusive (mutable) reference — "I borrow this and may modify it".
/// Calling `*cursor -= hh + 1.0` modifies the caller's variable in place.
/// This is the Rust equivalent of passing a pointer to a float in C.
fn draw_col_headers(
    layer:      &PdfLayerReference,
    bold:       &IndirectFontRef,
    settings:   &Settings,
    cursor:     &mut f32,
    has_status: bool,
    gen_md5:    bool,
    gen_xxh:    bool,
    accent1:    (f32, f32, f32),
) {
    // Header band height: 8 mm fits FS_HEAD (7 pt ≈ 2.5 mm) with ~2.7 mm top/bottom padding.
    let hh: f32 = 8.0;

    // Fill the full-width dark blue rectangle spanning the header band.
    // Top = *cursor, bottom = *cursor - hh.
    fill_rect(layer, M, *cursor - hh, W - M, *cursor,
              accent1.0, accent1.1, accent1.2);

    // Switch to white for text on the dark background.
    set_color(layer, TEXT_WHITE);

    // Vertical text baseline: 5.5 mm from the top of the band.
    // (8 mm band - 2.5 mm font height - 0 mm top gap ≈ 5.5 mm from top edge)
    let hy = *cursor - 5.5;

    // "Preview" label for the thumbnail column — always present.
    layer.use_text("Preview", FS_HEAD, Mm(M + 1.5), Mm(hy), bold);

    // Data column labels: retrieved from `active_col_names` in the same order
    // as the column widths from `col_widths`. Both functions check the same
    // `settings.col_*` flags in the same sequence, ensuring they stay in sync.
    let names  = active_col_names(settings, gen_md5, gen_xxh);
    let widths = col_widths(settings, gen_md5, gen_xxh);

    // `tx` advances left-to-right across the page as labels are drawn.
    // Start after the thumbnail column (M + TW) plus 2 mm gap.
    let mut tx = M + TW + 2.0;

    if has_status {
        // "St." is the abbreviated header for the "Status" column.
        // The full word "Status" would not fit in the 10 mm column at 7 pt.
        layer.use_text("St.", FS_HEAD, Mm(tx + 2.0), Mm(hy), bold);
        tx += 10.0; // advance past the 10 mm status column
    }

    // `names.iter().zip(widths.iter())` : pair each label with its column width.
    // `.zip()` stops at the shorter iterator — both have the same length since they
    // are both derived from the same `settings.col_*` flags.
    for (name, w) in names.iter().zip(widths.iter()) {
        layer.use_text(*name, FS_HEAD, Mm(tx), Mm(hy), bold);
        // `*name` dereferences `&&str` (reference to a `&'static str`) to `&str`.
        // Rust iterators yield references; `names.iter()` gives `&&str` here.
        tx += w; // `w` is `&f32` from `widths.iter()`; `tx += w` works via Deref
    }

    // Advance cursor past the band + 1 mm gap before the first data row.
    *cursor -= hh + 1.0;
}

/// Draws the page footer: a cyan rule, page number, file count, and app credit.
///
/// ### Why drawn after all rows?
/// The footer says "Page N / M". We only know M (total pages) after all data rows
/// have been placed. So footers are drawn in a separate pass at the end of `write_pdf`.
///
/// ### `crate::VERSION`
/// `crate::` refers to the root of the current crate (`main.rs`).
/// `VERSION` is a `pub const &str` defined there. Accessing it here avoids
/// duplicating the version string across multiple files.
fn draw_footer(
    layer:      &PdfLayerReference,
    reg:        &IndirectFontRef,
    page_num:   usize,
    page_total: usize,
    file_count: usize,
    accent1:    (f32, f32, f32),
) {
    fill_rect(layer, M, M + 1.5, W - M, M + 2.2,
              accent1.0, accent1.1, accent1.2);

    // Switch to medium grey for footer text — secondary, non-distracting.
    // The tuple `(0.55, 0.55, 0.58)` is passed directly to `set_color`.
    set_color(layer, (0.55, 0.55, 0.58));

    // Page number: approximately centred on the page width.
    // `W / 2.0 - 8.0` : rough centring. A "Page X / Y" string is ~16 mm wide
    // at 6 pt, so we start 8 mm left of centre.
    let page_str = format!("Page {} / {}", page_num, page_total);
    layer.use_text(&page_str, 6.0, Mm(W / 2.0 - 8.0), Mm(M - 1.5), reg);

    // App credit: 4 mm below the page number, slightly smaller font.
    // Positioned at `M - 5.5` mm (close to the bottom margin).
    let made_str = format!("Made with Bartleby {}", crate::VERSION);
    layer.use_text(&made_str, 5.5, Mm(W / 2.0 - 14.0), Mm(M - 5.5), reg);

    // File count: right-aligned, shown only on the last page.
    // `page_num == page_total` : equality check. In Rust, `==` is used for all types;
    // there is no `===` (strict equality) — Rust has no implicit type coercion.
    if page_num == page_total {
        let count_str = format!("{} file(s)", file_count);
        layer.use_text(&count_str, 6.0, Mm(W - M - 20.0), Mm(M - 1.5), reg);
    }
}

// ── Column helpers ────────────────────────────────────────────────────────────

/// Returns the column header label strings for all enabled columns, in order.
///
/// ### `&'static str` return type
/// String literals like `"Name"`, `"Type"` have type `&'static str`.
/// The lifetime `'static` means they live for the entire program duration (they are
/// embedded in the binary). Returning `Vec<&'static str>` is zero-copy — no heap
/// allocation for the strings themselves, only for the Vec's backing array.
///
/// ### Column order
/// The order here must exactly match the order in `active_cols()` and `col_widths()`.
/// All three functions iterate the same `settings.col_*` flags in the same sequence.
/// If you add a column, add it in all three functions at the same position.
fn active_col_names(s: &Settings, gen_md5: bool, gen_xxh: bool) -> Vec<&'static str> {
    let mut v = Vec::new();
    if s.col_name        { v.push("Name"); }
    if s.col_type        { v.push("Type"); }
    if s.col_size        { v.push("Size"); }
    if s.col_resolution  { v.push("Resolution"); }
    if s.col_codec       { v.push("Codec"); }
    if s.col_duration    { v.push("Duration"); }
    if s.col_bit_depth   { v.push("Bit Depth"); }
    if s.col_chroma      { v.push("Chroma"); }
    if s.col_color_space { v.push("Color Space"); }
    if s.col_sample_rate { v.push("Sample Rate"); }
    // Single checksum column — label reflects the active algorithm(s).
    if gen_md5 && gen_xxh { v.push("Checksum"); }
    else if gen_md5        { v.push("MD5"); }
    else if gen_xxh        { v.push("XXH3"); }
    // NOTE: "Status" is not listed here. The status column is drawn separately
    // (before the data columns) because it is not user-configurable in Settings —
    // it appears automatically whenever verify_ok is Some(…).
    v
}

/// Returns the cell values for one file row, for all enabled columns.
///
/// The returned Vec<String> is parallel to `active_col_names()` and `col_widths()`:
/// index 0 of this Vec corresponds to index 0 of the names and widths Vecs.
///
/// ### `md5: &str` parameter
/// The MD5 hash is passed separately from `meta` because `FileMeta` does not store
/// the hash (hashing is not a metadata concern — it is a copy-engine concern).
fn active_cols(
    meta:    &FileMeta,
    md5:     &str,      // MD5 hash string, empty if not computed
    xxh3:    &str,      // XXH3 hash string, empty if not computed
    s:       &Settings,
    gen_md5: bool,      // true → include MD5 column value
    gen_xxh: bool,      // true → include XXH3 column value
) -> Vec<String> {
    let mut v = Vec::new();
    // Each field is cloned from the FileMeta reference into an owned String.
    if s.col_name        { v.push(meta.name.clone()); }
    if s.col_type        { v.push(meta.file_type.clone()); }
    if s.col_size        { v.push(meta.size_human.clone()); }
    if s.col_resolution  { v.push(meta.resolution.clone()); }
    if s.col_codec       { v.push(meta.codec.clone()); }
    if s.col_duration    { v.push(meta.duration.clone()); }
    if s.col_bit_depth   { v.push(meta.bit_depth.clone()); }
    if s.col_chroma      { v.push(meta.chroma.clone()); }
    if s.col_color_space { v.push(meta.color_space.clone()); }
    if s.col_sample_rate { v.push(meta.sample_rate.clone()); }
    // Single checksum cell: one or two hash values depending on what was computed.
    // When both MD5 and XXH3 are present, they are joined with a newline so the
    // PDF renderer can split them across two lines within the same cell.
    // "MD5: " and "XXH3: " prefixes make the values unambiguous when both appear.
    if gen_md5 && gen_xxh {
        v.push(format!("MD5:  {}
XXH3: {}", md5, xxh3));
    } else if gen_md5 {
        v.push(md5.to_string());
    } else if gen_xxh {
        v.push(xxh3.to_string());
    }
    v
}

/// Returns the width (mm) of each enabled column, in the same order as the other two helpers.
///
/// ### Width budget
/// Total usable width = W - 2×M = 273 mm.
/// Thumbnail column = TW = 22 mm.
/// Remaining for data columns = 251 mm.
///
/// Sum of all column widths when all enabled:
/// 40 + 13 + 18 + 20 + 16 + 15 + 14 + 15 + 18 + 17 + 68 = 254 mm.
/// The 3 mm excess is acceptable — with Status column hidden, text rarely reaches
/// the right edge, and `clip()` prevents actual overflow.
///
/// The MD5 column is 68 mm because a 32-character hash at 6.5 pt Helvetica requires
/// approximately 32 × 6.5 pt × 0.3528 mm/pt × 0.50 char_ratio ≈ 36.7 mm minimum.
/// We use 68 mm to display without truncation and give visual breathing room.
fn col_widths(s: &Settings, gen_md5: bool, gen_xxh: bool) -> Vec<f32> {
    let mut v = Vec::new();
    if s.col_name        { v.push(40.0_f32); } // Name — widest; needs 2 lines for long filenames
    if s.col_type        { v.push(13.0_f32); } // Type — short extension (MP4, MXF, WAV…)
    if s.col_size        { v.push(18.0_f32); } // Size — "2.34 GB" is ~7 chars
    if s.col_resolution  { v.push(20.0_f32); } // Resolution — "4096x3072" is 9 chars
    if s.col_codec       { v.push(16.0_f32); } // Codec — "ProRes 422 HQ" is 13 chars
    if s.col_duration    { v.push(15.0_f32); } // Duration — "01:32:07" is 8 chars
    if s.col_bit_depth   { v.push(14.0_f32); } // Bit Depth — "10 bit" is 6 chars
    if s.col_chroma      { v.push(15.0_f32); } // Chroma — "4:2:2" is 5 chars
    if s.col_color_space { v.push(18.0_f32); } // Color Space — "BT.2020" is 7 chars
    if s.col_sample_rate { v.push(17.0_f32); } // Sample Rate — "96 kHz" is 6 chars
    // Single "Checksum" column — present if either MD5 or XXH3 was computed.
    // Width 52 mm fits a 32-char hash at ~6pt font (≈ 1.5 mm/char × 32 + margin).
    // When both hashes appear on two lines, the column is tall enough (RH = 8 mm).
    if gen_md5 || gen_xxh { v.push(52.0_f32); }
    v
}

/// Returns a `(directory, filename)` sort key for a relative path, lower-cased.
/// Used to sort entries by directory then filename, case-insensitively.
fn sort_key_for_rel(rel: &str) -> (String, String) {
    let p = Path::new(rel);
    let dir = p.parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/").to_lowercase())
        .unwrap_or_default();
    let name = p.file_name()
        .map(|n| n.to_string_lossy().to_string().to_lowercase())
        .unwrap_or_default();
    (dir, name)
}

/// Draws an outlined folder icon centred at `(cx, cy)`, matching the app's ico-folder SVG path.
///
/// The original SVG (viewBox 0 0 24 24) occupies x∈[3,21] y∈[5,20] (18×15 units).
/// Points are scaled uniformly to fit inside an 11×9 mm bounding box, then mapped to
/// printpdf's Y-up coordinate system (SVG y=20 → PDF y_bottom, SVG y=5 → PDF y_top).
fn draw_folder_icon_pdf(
    layer: &PdfLayerReference,
    cx: f32, cy: f32,
    r: f32, g: f32, b: f32,
) {
    let svg_w: f32 = 18.0; // SVG width of shape (x: 21-3)
    let svg_h: f32 = 15.0; // SVG height of shape (y: 20-5)
    let max_w: f32 = 11.0; // bounding box width  (mm)
    let max_h: f32 =  9.0; // bounding box height (mm)
    let scale  = (max_w / svg_w).min(max_h / svg_h);

    let ox = cx - (svg_w * scale) / 2.0; // bottom-left x
    let oy = cy - (svg_h * scale) / 2.0; // bottom-left y (PDF Y-up)

    // Maps an SVG (xs, ys) coordinate to a PDF Point.
    // Y is flipped: SVG y=20 (bottom of shape) maps to PDF oy (bottom of icon).
    let pt = |xs: f32, ys: f32| -> (Point, bool) {
        (Point::new(Mm(ox + (xs - 3.0) * scale), Mm(oy + (20.0 - ys) * scale)), false)
    };

    // Simplified 7-point polygon — straight-line approximation of the SVG arcs:
    //   bottom-left → bottom-right → body upper-right
    //   → tab/body join → fold crease → tab top-left → body upper-left
    let pts = vec![
        pt( 3.0, 20.0), // body bottom-left
        pt(21.0, 20.0), // body bottom-right
        pt(21.0,  7.0), // body upper-right
        pt(12.5,  7.0), // where tab joins body
        pt(10.5,  5.0), // fold crease (angled top of tab)
        pt( 5.0,  5.0), // tab upper-left
        pt( 3.0,  7.0), // body upper-left
    ];

    layer.set_outline_color(Color::Rgb(Rgb::new(r, g, b, None)));
    layer.set_outline_thickness(1.2_f32);
    layer.add_polygon(Polygon {
        rings:         vec![pts],
        mode:          PaintMode::Stroke,
        winding_order: WindingOrder::NonZero,
    });
}

/// Draws a full-width directory separator row: light background, folder icon, directory label.
fn draw_dir_separator(
    layer:   &PdfLayerReference,
    bold:    &IndirectFontRef,
    dir:     &str,
    cursor:  f32,
    accent1: (f32, f32, f32),
) {
    fill_rect(layer, M, cursor - RH, W - M, cursor, 0.90, 0.93, 0.96);
    draw_hline(layer, M, W - M, cursor - RH);
    let icon_cx = M + 1.0 + TW / 2.0;
    let icon_cy = cursor - RH / 2.0;
    draw_folder_icon_pdf(layer, icon_cx, icon_cy, accent1.0, accent1.1, accent1.2);
    set_color(layer, TEXT_DARK);
    let ty = cursor - RH / 2.0 - 1.5;
    layer.use_text(dir, FS_HEAD + 0.5, Mm(M + TW + 3.0), Mm(ty), bold);
}

// ── Thumbnail dispatch ────────────────────────────────────────────────────────

/// Draws the best available thumbnail for `path` in the leftmost cell of a data row.
///
/// Works through a priority chain, falling back to the next strategy if the current
/// one fails (file not decodable, external tool not installed, etc.):
///
/// 1. **Image files** (`IMAGE_EXTS`) — try `image::open` directly.
///    Works for: JPEG, PNG, TIFF, WebP, BMP, GIF. Fails for most RAW formats.
/// 2. **Video files** (`VIDEO_EXTS`) — extract a frame at t=1 s via `ffmpeg`.
///    Works for any format `ffmpeg` can decode (H.264, ProRes, MXF, R3D, BRAW…).
/// 3. **Audio files** (`AUDIO_EXTS`) — render a waveform PNG via `ffmpeg showwavespic`.
///    Works for: WAV, FLAC, MP3, AIFF, AAC, ALAC…
/// 4. **All other files** — try to get the OS MIME icon via `python3 + gi`.
///    Falls back to a small coloured rectangle if `python3` or `gi` is unavailable.
///
/// ### Thumbnail cell geometry
/// ```text
/// row_y ──────────────────────────── ← top of row (= cursor at draw time)
///          ┌────────────────────┐
///          │   thumbnail image  │   vertically centred: y = row_y - RH + (RH-TH)/2
///          └────────────────────┘
/// row_y - RH ─────────────────────── ← bottom of row
/// ```
fn draw_thumb(
    doc:   &PdfDocumentReference,
    layer: &PdfLayerReference,
    path:  &Path,
    ext:   &str,
    row_y: f32,
    bg:    (f32, f32, f32),   // row background colour for alpha compositing
) {
    // X position: M (left margin) + 1 mm inset so the image doesn't touch the row rule.
    let x = M + 1.0;
    // Y position: vertically centred within the row.
    // `row_y - RH` = bottom of row. `(RH - TH) / 2.0` = top/bottom padding.
    // Together: `row_y - RH + (RH - TH) / 2.0` = bottom of the thumbnail cell.
    let y = row_y - RH + (RH - TH) / 2.0;

    // `.contains(&ext.as_ref())` :
    //   `IMAGE_EXTS` is `&[&str]`. `.contains(item)` checks if any element equals `item`.
    //   `ext.as_ref()` converts `&String` to `&str` (via the `AsRef<str>` trait).
    //   Since `ext` is already `&str` (from the caller), this is a no-op here,
    //   but it is written defensively for clarity.
    if IMAGE_EXTS.contains(&ext.as_ref()) {
        // Load as RGBA so we can composite the alpha channel onto the row background.
        // This avoids a black or white halo: transparent pixels take the row colour
        // (light grey for even rows, white for odd rows).
        let rgba_opt = ::image::open(path).ok().map(|i|
            i.resize(224, 144, FilterType::Triangle).into_rgba8()
        );
        if let Some(rgba) = rgba_opt {
            // `rgba_on_bg` composites RGBA onto the row background colour,
            // producing a plain RGB image ready for embedding.
            let rgb = rgba_on_bg(rgba, bg);
            embed_rgb(doc, layer, rgb, None, x, y, TW, TH);
            return;
        }
        // Fall through if the image could not be loaded (e.g. unsupported RAW).
    }
    if VIDEO_EXTS.contains(&ext.as_ref()) {
        if let Some(rgb) = video_frame(path) {
            embed_rgb(doc, layer, rgb, None, x, y, TW, TH);
            return;
        }
    }
    if AUDIO_EXTS.contains(&ext.as_ref()) {
        if let Some(rgb) = audio_wave(path) {
            embed_rgb(doc, layer, rgb, None, x, y, TW, TH);
            return;
        }
    }
    // All strategies failed (or the extension matches none of the lists).
    // Show an OS MIME icon or a coloured rectangle.
    draw_file_icon(doc, layer, ext, x, y);
}

/// Loads a still image and resizes it to at most 224×144 px for thumbnail use.
///
/// Returns `None` if the file cannot be opened or the format is not supported.
///
/// ### `::image::open(path)`
/// `::image` refers to the external `image` crate. The `::` prefix disambiguates
/// from any local module named `image`. `open()` tries to guess the format from
/// the file header (magic bytes), not the extension. Returns `io::Result<DynamicImage>`.
///
/// ### `.ok()?`
/// `.ok()` converts `Result<T, E>` to `Option<T>` (discarding the error).
/// `?` returns `None` from the function if the value is `None`.
/// Together, `.ok()?` means "if this fails, return None from `load_image`".
///
/// ### `.resize(224, 144, FilterType::Triangle)`
/// Scales the image to fit within a 224×144 bounding box while preserving aspect ratio.
/// `Triangle` is a bilinear filter: better quality than `Nearest` (pixelated),
/// faster than `Lanczos3` (slightly sharper). A good trade-off for thumbnails.
///
/// Composites an RGBA image onto a white background, producing an RGB image.
///
/// PDF's `ImageXObject` only supports `ColorSpace::Rgb` — no alpha channel.
/// Images with transparency (PNG logos, OS MIME icons rendered by GTK/Python)
/// would show black transparent regions if converted directly with `to_rgb8()`,
/// because the `image` crate's default compositing background is black.
///
/// This function composites each pixel's alpha against white (255, 255, 255):
///   `out = alpha/255 × fg + (1 − alpha/255) × 255`
///
/// Splits an RGBA image into RGB + optional greyscale SMask for PDF transparency.
///
/// PDF supports soft-mask (SMask) transparency: a separate greyscale XObject encodes
/// per-pixel opacity. The PDF viewer composites the RGB image with the SMask at
/// display time, respecting the actual background (row colour, page background).
/// This gives true transparency — no white halo, no black fill.
///
/// If all pixels are fully opaque, `smask` is `None` (no extra XObject, smaller PDF).
///
/// ## Usage
/// ```rust
/// let (rgb, smask_opt) = rgba_split(rgba);
/// let xobj = ImageXObject {
///     color_space: ColorSpace::Rgb,
///     image_data:  rgb.into_raw(),
///     smask:       smask_opt,  // Option<SMask> directly — no Box needed
///     ..
/// };
/// ```
/// Splits an RGBA image: composites alpha onto a given background colour.
///
/// ## Why not use printpdf's SMask?
/// printpdf 0.7's `SMask` struct does not expose an `image_data` field in its
/// public API, making it impossible to build one from raw alpha bytes at runtime.
/// This is a known limitation — the genpdf crate documents that transparency
/// is not currently renderable via printpdf.
///
/// ## Workaround: composite onto background colour
/// Instead of true PDF transparency, we pre-multiply the alpha against the
/// row background colour (passed as `bg: (f32, f32, f32)` in 0.0–1.0 range).
/// This means the thumbnail blends correctly with its row colour rather than
/// showing a black or white halo.
///
/// For thumbnails on alternating rows (ROW_EVEN / ROW_ODD), the caller passes
/// the correct background. For the PDF logo, `rgba_to_rgb_white` is used instead.
fn rgba_on_bg(rgba: ::image::RgbaImage, bg: (f32, f32, f32)) -> RgbImage {
    let (w, h) = rgba.dimensions();
    let mut rgb = RgbImage::new(w, h);
    let (br, bg_g, bb) = (bg.0 * 255.0, bg.1 * 255.0, bg.2 * 255.0);
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let a = pixel[3] as f32 / 255.0;
        let r = (a * pixel[0] as f32 + (1.0 - a) * br) as u8;
        let g = (a * pixel[1] as f32 + (1.0 - a) * bg_g) as u8;
        let b = (a * pixel[2] as f32 + (1.0 - a) * bb) as u8;
        rgb.put_pixel(x, y, ::image::Rgb([r, g, b]));
    }
    rgb
}


/// The result is a flat RGB image suitable for embedding in a PDF, where
/// transparent areas appear white (matching the page background).
///
/// ### Type note
/// `::image::RgbaImage` = `ImageBuffer<Rgba<u8>, Vec<u8>>`
/// `::image::RgbImage`  = `ImageBuffer<Rgb<u8>, Vec<u8>>`
/// Both are re-exported from the `image` crate with the `::image::` prefix
/// used throughout this file to avoid ambiguity with any local `image` module.
fn rgba_to_rgb_white(rgba: ::image::RgbaImage) -> RgbImage {
    let (w, h) = rgba.dimensions();
    let mut rgb = RgbImage::new(w, h);
    for (x, y, pixel) in rgba.enumerate_pixels() {
        // `pixel[3]` is the alpha channel (0 = fully transparent, 255 = opaque).
        let a = pixel[3] as f32 / 255.0;
        // Composite: out = a × fg + (1 − a) × 255 (white background).
        let r = (a * pixel[0] as f32 + (1.0 - a) * 255.0) as u8;
        let g = (a * pixel[1] as f32 + (1.0 - a) * 255.0) as u8;
        let b = (a * pixel[2] as f32 + (1.0 - a) * 255.0) as u8;
        // `image::Rgb([r, g, b])` wraps three u8 values in the `Rgb` pixel type.
        // `put_pixel` writes it at the (x, y) coordinate.
        // `image::Rgb` is re-exported from the `image` crate.
        // Since `use ::image::RgbImage` is in scope, we can access Rgb via the crate path.
        rgb.put_pixel(x, y, ::image::Rgb([r, g, b]));
    }
    rgb
}


/// Extracts a single video frame at t=1 s via `ffmpeg` and returns it as an `RgbImage`.
///
/// ### ffmpeg command constructed
/// ```sh
/// ffmpeg -y -ss 00:00:01 -i <path>
///        -map 0:v:0 -vframes 1
///        -vf "scale=224:144:force_original_aspect_ratio=decrease,
///             pad=224:144:(ow-iw)/2:(oh-ih)/2:color=black"
///        -pix_fmt rgb24 -q:v 3 /tmp/_bartleby_vthumb.jpg
/// ```
///
/// Key arguments explained:
/// - `-y` : overwrite the output file without asking.
/// - `-ss 00:00:01` : seek to 1 second before decoding.
///   Using 2 s or more risks failure for short clips. 1 s is safe for clips ≥ 1 s.
/// - `-i <path>` : input file (`.arg(path)` passes as `OsStr` — handles spaces safely).
/// - `-map 0:v:0` : select the first video stream of input 0.
///   MXF containers often contain multiple audio/data streams. Without `-map`,
///   ffmpeg might select an audio stream and produce a blank image.
/// - `-vframes 1` : extract exactly one frame.
/// - `-vf "scale=…,pad=…"` : scale to fit 224×144 (preserving aspect ratio),
///   then pad to exactly 224×144 with black bars (letterbox/pillarbox).
/// - `-pix_fmt rgb24` : force output to 8-bit RGB (3 bytes per pixel).
///   MXF H.264 High 4:2:2 produces `yuv422p10le` which `image::open()` cannot decode.
/// - `-q:v 3` : JPEG quality 3 (scale 1–31, lower=better). Balances quality and speed.
///
/// Returns `None` if `ffmpeg` is not installed, the file is unreadable, or the clip
/// is shorter than 1 second (seek fails). In debug builds, the last line of ffmpeg's
/// stderr is printed to the terminal for diagnostics.
fn video_frame(path: &Path) -> Option<RgbImage> {
    // Temporary file for the extracted JPEG frame.
    // `std::env::temp_dir()` : the OS temp directory (/tmp on Linux).
    // `.join(…)` : append a filename, giving e.g. `/tmp/_bartleby_vthumb.jpg`.
    let tmp = std::env::temp_dir().join("_bartleby_vthumb.jpg");

    // `Command::new("ffmpeg")` : build an ffmpeg command.
    // Each `.arg(…)` appends one argument. Do NOT concatenate into a single string
    // with spaces — `Command` passes arguments as a Vec to the OS, bypassing the shell.
    // Shell string parsing (splitting on spaces) is not performed. This means filenames
    // with spaces work correctly without escaping.
    // Build the ffmpeg command, passing it through no_window() to suppress
    // the cmd.exe console window flash on Windows (no-op on Linux/macOS).
    // no_window() takes &mut Command and returns &mut Command, so it fits
    // naturally into the builder chain.
    let mut ffmpeg_cmd = Command::new("ffmpeg");
    ffmpeg_cmd
        .arg("-y")                    // overwrite output without prompt
        .arg("-ss").arg("00:00:01")   // seek to 1 second before decoding
        .arg("-i").arg(path)          // input file (OsStr — handles any filename)
        .arg("-map").arg("0:v:0")     // first video stream of first input
        .arg("-vframes").arg("1")     // extract exactly one frame
        .arg("-vf").arg(
            "scale=224:144:force_original_aspect_ratio=decrease,\
             pad=224:144:(ow-iw)/2:(oh-ih)/2:color=black"
            // scale: fit within 224×144 preserving aspect ratio
            // pad: fill remaining area with black to reach exactly 224×144
        )
        .arg("-pix_fmt").arg("rgb24") // force 8-bit RGB output (decodable by `image` crate)
        .arg("-q:v").arg("3")         // JPEG quality (1–31, lower = better)
        .arg(&tmp);                   // output path
    let output = no_window(&mut ffmpeg_cmd).output().ok()?; // None if ffmpeg not installed

    if !output.status.success() {
        // ffmpeg exited with a non-zero code (e.g. corrupt file, seek failed).
        // `eprintln!` writes to stderr — visible in the terminal during `npm run dev`.
        eprintln!("[Bartleby] ffmpeg video_frame failed for: {}", path.display());
        eprintln!("[Bartleby] ffmpeg stderr: {}",
            String::from_utf8_lossy(&output.stderr)
                .lines()              // split into lines
                .last()               // take the last line (most informative)
                .unwrap_or("(empty)"));
        return None;
    }

    // Load the JPEG that ffmpeg wrote to /tmp.
    let rgb = ::image::open(&tmp).ok().map(|i| rgba_to_rgb_white(i.into_rgba8()));
    // Delete the temp file immediately after loading.
    // `let _ = …` discards the io::Result — failure to delete is non-critical.
    let _   = std::fs::remove_file(&tmp);
    rgb // return Option<RgbImage>
}

/// Renders an audio waveform image via ffmpeg's `showwavespic` filter.
///
/// ### ffmpeg command constructed
/// ```sh
/// ffmpeg -y -i <path>
///        -filter_complex "showwavespic=s=224x144:colors=#4dffd8|#2a6abf:scale=sqrt"
///        -frames:v 1 /tmp/_bartleby_wave.png
/// ```
///
/// The `showwavespic` filter produces a static waveform image:
/// - `s=224x144` : output image size in pixels.
/// - `colors=#4dffd8|#2a6abf` : two colours — the top/bottom halves of the waveform.
///   These are Bartleby's brand colours (teal and blue).
/// - `scale=sqrt` : apply square-root scaling to the amplitude axis.
///   This makes quiet passages more visible (linear scale compresses quiet audio).
///
/// `.stdout(Stdio::null()).stderr(Stdio::null())` : suppress all ffmpeg output.
/// We use `.status()` (returns only the exit code) instead of `.output()` (captures
/// stdout + stderr). This is slightly more efficient when we don't need the output.
///
/// Returns `None` if `ffmpeg` is not installed or the file cannot be decoded.
fn audio_wave(path: &Path) -> Option<RgbImage> {
    let tmp = std::env::temp_dir().join("_bartleby_wave.png");

    // Suppress console window on Windows, then run ffmpeg.
    let mut wave_cmd = Command::new("ffmpeg");
    wave_cmd
        .arg("-y")
        .arg("-i").arg(path)
        .arg("-filter_complex")
        .arg("showwavespic=s=224x144:colors=#4dffd8|#2a6abf:scale=sqrt")
        .arg("-frames:v").arg("1")
        .arg(&tmp)
        // Suppress ffmpeg's verbose output — we only need the exit status.
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let status = no_window(&mut wave_cmd).status().ok()?; // None if ffmpeg not installed

    if !status.success() { return None; }

    let rgb = ::image::open(&tmp).ok().map(|i| rgba_to_rgb_white(i.into_rgba8()));
    let _   = std::fs::remove_file(&tmp);
    rgb
}

/// Draws an OS MIME icon or a coloured fallback rectangle in the thumbnail cell.
///
/// ### Strategy
/// 1. Try `get_mime_icon(ext)` → runs a Python script via `python3 -c …` to get
///    the GTK icon theme icon for this file type (64×64 px PNG).
/// 2. If that fails: draw a small coloured square. The colour is chosen by file type
///    (Word = blue, Excel = green, PDF = red, etc.).
///
/// ### Why Python for icons?
/// GTK icon theme queries require GObject Introspection (`gi.repository`), which is
/// a Python binding to the GTK/GNOME C libraries. There is no mature Rust crate for
/// this. The Python script is short (<10 lines), runs quickly (~50 ms), and produces
/// the exact icon the user would see in the file manager for the same file type.
///
/// ### No text in the fallback square
/// At 20×16 mm and 6 pt, text would be illegible in the final printed PDF.
/// A coloured square is more visually useful — it gives a quick type hint by colour.
fn draw_file_icon(
    doc:   &PdfDocumentReference,
    layer: &PdfLayerReference,
    ext:   &str,   // lowercase file extension, e.g. "pdf", "docx", "zip"
    x:     f32,    // left edge of the thumbnail cell (mm)
    y:     f32,    // bottom edge of the thumbnail cell (mm)
) {
    if let Some(rgb) = get_mime_icon(ext) {
        // Convert the icon size from pixels to mm.
        // 64 px at 96 dpi: 64 / (96 / 25.4) = 64 / 3.779 ≈ 16.93 mm.
        // `.min(TH).min(TW)` : clamp to the cell dimensions so the icon never overflows.
        let icon_mm: f32 = (64.0_f32 / (96.0_f32 / 25.4_f32)).min(TH).min(TW);

        // Centre the icon in the cell horizontally and vertically.
        // `(TW - icon_mm) / 2.0` : half the remaining horizontal space.
        // `(TH - icon_mm) / 2.0` : half the remaining vertical space.
        let ox = x + (TW - icon_mm) / 2.0;
        let oy = y + (TH - icon_mm) / 2.0;
        embed_rgb(doc, layer, rgb, None, ox, oy, icon_mm, icon_mm);
        return; // icon drawn successfully — skip the fallback rectangle
    }

    // Fallback: coloured square (12×12 mm), centred in the cell.
    // Colours chosen to match common application brand colours for quick recognition.
    // `match ext { … }` matches on the `&str` value directly.
    // `|` in a match arm = OR: the arm matches if ext is either value.
    let (r, g, b): (f32, f32, f32) = match ext {
        "pdf"                           => (0.80, 0.10, 0.10), // Adobe red
        "doc"  | "docx"                 => (0.18, 0.42, 0.70), // Microsoft Word blue
        "xls"  | "xlsx"                 => (0.13, 0.54, 0.30), // Microsoft Excel green
        "ppt"  | "pptx"                 => (0.83, 0.34, 0.14), // Microsoft PowerPoint orange
        "zip"  | "tar" | "gz" | "rar" | "7z" => (0.55, 0.42, 0.14), // archive brown
        "txt"  | "md"                   => (0.50, 0.50, 0.55), // text grey-blue
        _                               => (0.38, 0.38, 0.44), // generic dark grey
    };
    let sq: f32 = 12.0; // square size in mm
    // Centre the square in the cell.
    // Left edge:  x + (TW - sq) / 2.0
    // Right edge: x + (TW + sq) / 2.0
    // (Same formula as (x + TW/2 - sq/2) and (x + TW/2 + sq/2))
    fill_rect(layer,
              x + (TW - sq) / 2.0, y + (TH - sq) / 2.0,
              x + (TW + sq) / 2.0, y + (TH + sq) / 2.0,
              r, g, b);
}

/// Retrieves a 64×64 px MIME icon from the OS GTK icon theme, via a Python script.
///
/// ### Why a Python script?
/// GTK icon theme lookups require the `gi.repository` (GObject Introspection) Python
/// bindings to the GNOME C libraries. There is no Rust crate for this on Linux.
/// The script is short, starts in ~30 ms, and returns the *exact* icon the file
/// manager (Nautilus, Nemo, Thunar) would show for the same file type.
///
/// ### Script steps
/// 1. Convert the extension to a MIME type (e.g. "docx" → "application/vnd.openxmlformats…").
/// 2. Get the icon for that MIME type from the GTK icon registry.
/// 3. Look up the icon in the current desktop theme (Mint-Y, Adwaita, Yaru…).
/// 4. Scale to 64×64 px and save as PNG to a temp file.
///
/// ### Inline Python with `r#"…"#`
/// Rust raw string literals start with `r` and any number of `#` characters.
/// Inside a raw string, backslashes and quotes are literal — no escape sequences.
/// This is ideal for embedding Python code which uses `'` for strings.
///
/// ### `format!(r#"…{mime}…{path}…"#, mime = mime, path = …)`
/// Named format arguments: `{mime}` is replaced by the value of `mime`,
/// `{path}` by the temp file path. This is equivalent to Python's f-strings.
///
/// ### AUDIT NOTE — injection risk
/// The `{path}` value is embedded inside a Python string literal.
/// If `std::env::temp_dir()` returned a path containing a single quote (`'`),
/// it would break the Python string. We mitigate this by using a hard-coded,
/// safe filename `_bartleby_icon_{ext}.png` where `ext` is always an ASCII
/// alphabetic string (file extensions are safe).
fn get_mime_icon(ext: &str) -> Option<RgbImage> {
    let mime = ext_to_mime(ext); // e.g. "pdf" → "application/pdf"
    let tmp  = std::env::temp_dir().join(format!("_bartleby_icon_{}.png", ext));

    // Build the Python script as a String using a raw string literal.
    // `r#" … "#` : raw string literal — backslashes and single quotes are literal.
    // Named format args: `{mime}` and `{path}` are substituted at runtime.
    let script = format!(
        r#"import gi, sys
gi.require_version('Gtk', '3.0')
from gi.repository import Gio, Gtk, GdkPixbuf
ct = Gio.content_type_from_mime_type('{mime}')
if not ct: sys.exit(1)
icon = Gio.content_type_get_icon(ct)
theme = Gtk.IconTheme.get_default()
info = theme.lookup_by_gicon(icon, 64, 0)
if not info: sys.exit(1)
pb = info.load_icon()
if not pb: sys.exit(1)
pb = pb.scale_simple(64, 64, GdkPixbuf.InterpType.BILINEAR)
pb.savev('{path}', 'png', [], [])
"#,
        mime = mime,
        path = tmp.to_str().unwrap_or(""), // &Path → &str (fails for non-UTF-8 paths)
    );

    // Run `python3 -c "<script>"`. `.output()` waits and captures stdout/stderr.
    // `.ok()?` : None if `python3` is not installed.
    // Suppress console window on Windows before spawning python3.
    // python3 is only available on Linux/macOS (the GTK icon API used in the script
    // requires gi.repository which does not exist on Windows). On Windows this call
    // will return None (python3 not found or script fails) and draw_file_icon()
    // will fall through to the coloured rectangle fallback. The no_window() call
    // here is defensive — it ensures no flash even if python3 is somehow present.
    let mut py_cmd = Command::new("python3");
    py_cmd.arg("-c").arg(&script);
    let out = no_window(&mut py_cmd).output().ok()?;

    // Check both the exit code and that the file was actually created.
    // `tmp.exists()` verifies the PNG was written (the script may succeed but write nothing).
    if out.status.success() && tmp.exists() {
        let rgb = ::image::open(&tmp).ok().map(|i| rgba_to_rgb_white(i.into_rgba8()));
        let _   = std::fs::remove_file(&tmp); // clean up the temp file
        return rgb;
    }
    None // Python script failed or icon file was not created
}

/// Maps common file extensions to their canonical MIME type strings.
///
/// Used by `get_mime_icon` to look up the OS icon for a given file type.
///
/// ### `&str` return type
/// All return values are string literals with `'static` lifetime — no heap allocation.
///
/// ### Fallback: `"application/octet-stream"`
/// This is the generic binary MIME type. GTK typically maps it to a plain file icon.
/// It is used for any extension not explicitly listed.
fn ext_to_mime(ext: &str) -> &str {
    match ext {
        "pdf"          => "application/pdf",
        "doc"          => "application/msword",
        "docx"         => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls"          => "application/vnd.ms-excel",
        "xlsx"         => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt"          => "application/vnd.ms-powerpoint",
        "pptx"         => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "zip"          => "application/zip",
        "tar"          => "application/x-tar",
        "gz"           => "application/gzip",
        "rar"          => "application/x-rar-compressed",
        "7z"           => "application/x-7z-compressed",
        "txt" | "md"   => "text/plain",       // `|` in match: OR — matches either
        "xml"          => "application/xml",
        "json"         => "application/json",
        "html" | "htm" => "text/html",
        "svg"          => "image/svg+xml",
        "py"           => "text/x-python",
        "rs"           => "text/x-rust",
        _              => "application/octet-stream", // unknown type → generic binary
    }
}

// ── Drawing primitives ────────────────────────────────────────────────────────
//
// These four functions form the low-level drawing API for this module.
// They wrap printpdf's somewhat verbose API into concise, reusable helpers.

/// Embeds an `RgbImage` into the PDF at `(x, y)` mm (bottom-left), scaled uniformly
/// to fit within the `max_w × max_h` mm bounding box (letterbox, no distortion).
///
/// ### Coordinate convention
/// `(x, y)` is the BOTTOM-LEFT corner of the image in PDF's Y-up coordinate system.
/// An image at `y = 50` has its bottom edge 50 mm from the page bottom.
///
/// ### Aspect-ratio-preserving scale calculation
/// ```text
/// px_per_mm  = dpi / 25.4               (96 dpi → 3.779 px per mm)
/// img_w_mm   = image_width_px  / px_per_mm   (pixels → mm at 96 dpi)
/// img_h_mm   = image_height_px / px_per_mm
///
/// sx = max_w / img_w_mm   (scale needed to fill horizontal space)
/// sy = max_h / img_h_mm   (scale needed to fill vertical space)
///
/// scale = min(sx, sy)     (use the smaller to avoid overflowing either axis)
/// ```
/// This is the "object-fit: contain" CSS behaviour: the image fills the bounding box
/// as much as possible without exceeding it in either dimension.
///
/// ### `_doc` parameter
/// Prefixing with `_` tells the compiler (and readers) that this parameter is
/// intentionally unused in the function body. Without `_`, the compiler would emit
/// a `dead_code` warning. The parameter is kept for potential future use (e.g. if
/// printpdf requires the document handle for embedded resources in a future version).
///
/// ### `rgb.into_raw()`
/// `into_raw()` consumes the `RgbImage` and returns its underlying `Vec<u8>`.
/// This is a zero-copy transfer of the pixel data from the image buffer to printpdf.
/// The raw bytes are in row-major order: R₀G₀B₀ R₁G₁B₁ … (left to right, top to bottom).
fn embed_rgb(
    _doc:  &PdfDocumentReference,
    layer: &PdfLayerReference,
    rgb:   RgbImage,                         // RGB pixel data
    smask: Option<SMask>,                   // pre-computed alpha SMask, or None
    x:     f32,
    y:     f32,
    max_w: f32,
    max_h: f32,
) {
    let (pw, ph) = (rgb.width(), rgb.height());

    // Use pre-computed smask if provided; otherwise no transparency.
    // embed_rgb receives a plain RgbImage (alpha already handled by caller via
    // rgba_split or discarded for JPEG/video thumbnails that have no alpha).
    let (rgb_img, smask_xobj) = (rgb, smask);
    let xobj = ImageXObject {
        width:              Px(pw as usize),
        height:             Px(ph as usize),
        color_space:        ColorSpace::Rgb,
        bits_per_component: ColorBits::Bit8,
        interpolate:        true,
        image_data:         rgb_img.into_raw(),
        image_filter:       None,
        // `smask` field expects `Option<SMask>` directly — no Box needed.
        // rgba_split() already returns Option<SMask> (the correct type).
        smask:              smask_xobj,
        clipping_bbox:      None,
    };
    let img = Image::from(xobj);

    // Compute the uniform scale factor (fit mode, no distortion).
    let dpi: f32  = 96.0;              // reference DPI: how many pixels per inch the image was rendered at
    let px_per_mm = dpi / 25.4;        // pixels per mm (1 inch = 25.4 mm → 96 px / 25.4 mm ≈ 3.779 px/mm)
    let sx        = max_w / (pw as f32 / px_per_mm); // scale to fit width
    let sy        = max_h / (ph as f32 / px_per_mm); // scale to fit height
    let scale     = sx.min(sy);        // use the smaller: "contain" mode

    // Add the image to the PDF layer with the computed transform.
    // `layer.clone()` : printpdf requires an owned `PdfLayerReference` here.
    // Cloning a `PdfLayerReference` is cheap — it is Arc-backed, so it just increments a counter.
    img.add_to_layer(layer.clone(), ImageTransform {
        translate_x: Some(Mm(x)),    // X position (left edge)
        translate_y: Some(Mm(y)),    // Y position (bottom edge, Y-up)
        scale_x:     Some(scale),    // horizontal scale factor
        scale_y:     Some(scale),    // vertical scale factor (same → no distortion)
        rotate:      None,           // no rotation
        dpi:         Some(dpi),      // reference DPI for printpdf's internal scaling
    });
}

/// Fills a solid-colour axis-aligned rectangle.
///
/// ### Why `add_polygon` and not a simpler API?
/// printpdf 0.7 removed the `add_rect` helper and changed the `Line`/`add_shape`
/// API that existed in 0.5/0.6. The `add_polygon` API with `PaintMode::Fill` and
/// a 4-point ring is the correct approach in 0.7 for solid rectangles.
///
/// ### Parameter convention
/// `(x1, y1)` = bottom-left corner, `(x2, y2)` = top-right corner.
/// (In printpdf's Y-up system, y1 < y2 means y1 is the lower edge.)
///
/// ### `PaintMode::Fill`
/// The polygon is filled with the current fill colour and no visible stroke.
/// We set both fill and outline to the same colour to avoid any sub-pixel stroke artefacts.
///
/// ### `WindingOrder::NonZero`
/// The non-zero fill rule: points inside simple (non-self-intersecting) polygons are filled.
/// For a convex rectangle this is equivalent to the even-odd rule.
///
/// ### Points and `false`
/// `(Point, bool)` : each vertex + a boolean for "is this a Bézier control point?".
/// `false` = straight line to this point (not a curve). We need 4 straight-line vertices
/// to form a rectangle.
fn fill_rect(
    layer: &PdfLayerReference,
    x1: f32, y1: f32, x2: f32, y2: f32,   // bottom-left and top-right corners
    r: f32, g: f32, b: f32,                // fill colour (normalised RGB)
) {
    // Set the fill colour for the polygon interior.
    layer.set_fill_color(Color::Rgb(Rgb::new(r, g, b, None)));
    // Set the outline (stroke) colour to the same colour.
    // Without this, printpdf might use a default stroke colour that creates a visible border.
    layer.set_outline_color(Color::Rgb(Rgb::new(r, g, b, None)));

    // Define the four corners of the rectangle (clockwise order).
    let pts = vec![
        (Point::new(Mm(x1), Mm(y1)), false), // bottom-left
        (Point::new(Mm(x2), Mm(y1)), false), // bottom-right
        (Point::new(Mm(x2), Mm(y2)), false), // top-right
        (Point::new(Mm(x1), Mm(y2)), false), // top-left
    ];

    layer.add_polygon(Polygon {
        rings: vec![pts],               // one ring = one closed shape
        mode:  PaintMode::Fill,         // fill only (no stroke)
        winding_order: WindingOrder::NonZero,
    });
}

/// Draws a thin (0.3 mm tall) horizontal line using a filled rectangle.
///
/// A "line" in PDF is normally drawn with a stroke path, but printpdf 0.7's stroke
/// API changed between versions. A 0.3 mm tall `fill_rect` is the reliable equivalent.
/// At typical print resolution (300 dpi), 0.3 mm ≈ 3.5 pixels — clearly visible.
///
/// Colour: `(0.82, 0.86, 0.90)` = a light silver-grey (#D0DBDA), subtle enough not
/// to compete with the data but visible enough to delineate rows.
fn draw_hline(layer: &PdfLayerReference, x1: f32, x2: f32, y: f32) {
    // `y` = the Y of the top edge of the line.
    // `y + 0.3` = the Y of the bottom edge (0.3 mm below).
    fill_rect(layer, x1, y, x2, y + 0.3, 0.82, 0.86, 0.90);
}

/// Sets the active fill colour for all subsequent drawing operations on this layer.
///
/// ### `layer.set_fill_color(…)`
/// In PDF, colours are part of the graphics state. Setting the fill colour here affects
/// all subsequent `use_text(…)` and `add_polygon(…)` calls until the colour is changed again.
/// Text rendering uses the fill colour (not the stroke colour) for the character bodies.
///
/// ### `(f32, f32, f32)` tuple parameter
/// The colour is passed as a plain 3-tuple, matching the `TEXT_DARK`, `TEXT_MID`,
/// `TEXT_WHITE`, etc. constants defined at the top of this file. This avoids
/// constructing a `Color::Rgb(Rgb::new(…))` at every call site.
fn set_color(layer: &PdfLayerReference, c: (f32, f32, f32)) {
    layer.set_fill_color(Color::Rgb(Rgb::new(c.0, c.1, c.2, None)));
    // `c.0`, `c.1`, `c.2` : tuple field access by index (Rust tuples use `.0`, `.1`, `.2`).
    // `None` : optional ICC colour profile — not used here.
}

/// Clips `text` to fit within `col_w` mm at font size `fs` points, appending `"…"` if truncated.
///
/// ### Character width estimation
/// Helvetica Regular character width ≈ `fs × 0.155` mm (empirically derived).
/// This accounts for the average width of mixed-case ASCII text at various sizes.
/// The factor 0.155 = 0.3528 mm/pt × 0.44 char_width_ratio ≈ 0.155 mm/pt.
///
/// ### Why estimate instead of exact metrics?
/// Exact character widths require querying the font's kerning and advance width tables.
/// printpdf does not expose these. The 0.155 factor is accurate enough for layout:
/// in the worst case (all narrow characters like `iiiii`), the clip is too conservative.
/// The alternative (overflow into the adjacent column) is worse than over-clipping.
///
/// ### `text.chars().count()` vs `text.len()`
/// `.len()` returns the number of **bytes** in a UTF-8 string.
/// `.chars().count()` returns the number of **Unicode codepoints** (characters).
/// For ASCII text they are equal, but for multibyte characters (é = 2 bytes, 1 char)
/// they differ. We use `.chars().count()` to correctly handle non-ASCII filenames.
///
/// ### `char_indices()` and `nth()`
/// `text.char_indices()` yields `(byte_offset, char)` pairs.
/// `.nth(n)` seeks to the nth element — O(n) for UTF-8 strings.
/// We need the byte offset (not the char index) because Rust string slicing (`&text[..end]`)
/// uses byte offsets. Slicing at a char boundary is valid; slicing mid-character panics.
fn clip(text: &str, col_w: f32, fs: f32) -> String {
    // Estimate how many characters fit in `col_w` mm.
    let max_chars = (col_w / (fs * 0.155)) as usize;
    if text.chars().count() <= max_chars {
        return text.to_string(); // text fits — return as-is
    }
    // Find the byte offset of the character at position `max_chars - 1`.
    // We subtract 1 to leave room for the "…" suffix.
    let end = text.char_indices()
        .nth(max_chars.saturating_sub(1)) // `saturating_sub`: clamps to 0 if max_chars == 0
        .map(|(i, _)| i)                  // extract the byte offset
        .unwrap_or(text.len());            // fall back to end of string
    // `&text[..end]` : a &str slice from byte 0 to byte `end` (exclusive).
    // This is valid UTF-8 because `end` is always a char boundary from `char_indices`.
    format!("{}…", &text[..end])          // append the ellipsis character U+2026
}

/// Splits `text` into at most 2 lines for the Name column, returning `(line1, Option<line2>)`.
///
/// This is specifically designed for camera filenames which follow patterns like:
/// `"A001C001_240115_RJMF.mov"` or `"IMG_20240115_143245_HDR.jpg"` — long names
/// with no natural word boundaries that would benefit from breaking at underscores.
///
/// ### Split strategy
/// 1. If `text` is ≤ `MAX_NAME_CHARS` chars, return `(text, None)` — no wrapping needed.
/// 2. Find the byte position of character `MAX_NAME_CHARS`.
/// 3. Try to split at the last space (' ') before that position (`rfind` on the slice).
///    If no space found, split at the character boundary exactly.
/// 4. Return `(line1, Some(clip(rest, col_w, fs)))` — line2 is clipped if still too long.
///
/// ### `rfind(' ')`
/// `str::rfind(pattern)` returns the byte offset of the *last* occurrence of `pattern`.
/// We search in `text[..boundary]` (the first `MAX_NAME_CHARS` chars) to find the
/// last space before the cut point. This gives a more natural line break than a hard cut.
///
/// ### `.trim_end()` / `.trim_start()`
/// After splitting at a space, `line1` may have a trailing space and `rest` a leading space.
/// `.trim_end()` removes trailing whitespace; `.trim_start()` removes leading whitespace.
/// We call `.to_string()` to get owned `String` values from the `&str` slices.
fn wrap_text(text: &str, col_w: f32, fs: f32) -> (String, Option<String>) {
    // Maximum characters on the first line (fixed threshold for visual consistency).
    const MAX_NAME_CHARS: usize = 25;

    if text.chars().count() <= MAX_NAME_CHARS {
        return (text.to_string(), None); // text fits on one line
    }

    // Find the byte offset of the character at position MAX_NAME_CHARS.
    // This is where we will cut the string if no space is found earlier.
    let boundary = text.char_indices()
        .nth(MAX_NAME_CHARS)           // the character at position 25 (0-based)
        .map(|(i, _)| i)               // extract byte offset
        .unwrap_or(text.len());        // if nth returns None (text is shorter), use the end

    // Try to split at the last space before `boundary`.
    // `text[..boundary]` : the first `MAX_NAME_CHARS` bytes (valid UTF-8 slice).
    // `.rfind(' ')` : byte offset of the last space in that slice, or None.
    // `.unwrap_or(boundary)` : if no space, cut at the char boundary exactly.
    let split = text[..boundary].rfind(' ').unwrap_or(boundary);

    // Construct the two lines.
    let line1 = text[..split].trim_end().to_string();
    // `text[split..]` : the remainder of the string starting at `split` (byte offset).
    // `.trim_start()` : remove leading whitespace (the space we split at).
    let rest  = text[split..].trim_start();

    let line2 = if rest.is_empty() {
        None // Nothing left after trimming — one line is sufficient
    } else {
        // Clip line 2 if it is still too long for the column.
        Some(clip(rest, col_w, fs))
    };

    (line1, line2)
}
