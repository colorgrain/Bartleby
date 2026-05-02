//! # Module `metadata`
//!
//! Extracts technical metadata from media files via the `mediainfo` CLI tool
//! and generates CSV reports from the collected data.
//!
//! ## Why `mediainfo --Inform` instead of a Rust crate?
//!
//! `mediainfo` is the industry-standard tool for media file analysis. It natively
//! supports professional formats including:
//! - R3D (RED Cinema), BRAW (Blackmagic RAW)
//! - MXF (Sony XDCAM, Panasonic P2, AS-11 broadcast)
//! - ProRes, DNxHD, LOG formats from most cinema cameras
//! - The majority of RAW still formats (CR3, NEF, ARW, DNG…)
//!
//! No actively maintained Rust crate covers these formats reliably.
//!
//! The `--Inform` option of mediainfo provides a structured query interface:
//! you specify exactly which fields you want and receive a pipe-delimited response.
//! This avoids parsing a full XML/JSON output.
//!
//! Example query and response:
//! ```text
//! mediainfo --Inform="Video;%Width%|%Height%|%Format%|%BitDepth%" clip.mp4
//! → "1920|1080|AVC|8"
//! ```
//!
//! If `mediainfo` is not installed, every call to `run_inform()` returns `None`
//! and all technical fields remain empty strings. Copy and checksum verification
//! operations are not affected.
//!
//! ## mediainfo stream sections
//! - `General` — container-level info (total duration, container format)
//! - `Video`   — video track (resolution, codec, chroma, colour space, bit depth)
//! - `Audio`   — audio track (codec, sample rate, bit depth)
//! - `Image`   — image track (JPEG, PNG, TIFF, most RAW formats)
//!
//! ## Windows console window suppression
//!
//! On Windows, `std::process::Command::new("mediainfo")` spawns a new process.
//! By default, Windows creates a visible console window (cmd.exe) for each
//! spawned process, which flashes briefly on screen during every mediainfo call.
//! The `no_window()` helper function applies the `CREATE_NO_WINDOW` flag
//! (0x08000000) via the Windows-specific `CommandExt` trait to suppress this.
//! On Linux and macOS, `no_window()` is a no-op.

use std::path::Path;
use std::process::Command;
use crate::settings::Settings;
use chrono::Local;

// ── Windows console suppression ───────────────────────────────────────────────
//
// On Windows, spawning a child process (mediainfo, ffmpeg, python3) creates a
// visible cmd.exe console window that flashes on screen for a fraction of a
// second. This is distracting and looks broken to users.
//
// The fix: pass the CREATE_NO_WINDOW flag (value 0x08000000) to the Windows
// CreateProcess API via the CommandExt trait from std::os::windows::process.
//
// `#[cfg(target_os = "windows")]` is a conditional compilation attribute:
// the `use` statement is only compiled into the binary when building for Windows.
// On Linux and macOS, this import is absent — the trait does not exist on those
// platforms and would cause a compile error if included unconditionally.
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// Suppresses the console window that Windows creates when spawning child processes.
///
/// ## The problem
/// On Windows, every `Command::new("mediainfo")` call spawns a cmd.exe console
/// window that flashes briefly on screen. With dozens of files being processed in
/// parallel (via rayon), this causes a visually distracting storm of flickering
/// windows during the metadata extraction phase.
///
/// ## The solution
/// The Windows `CreateProcess` API accepts a `dwCreationFlags` parameter.
/// `CREATE_NO_WINDOW` (0x08000000) instructs Windows to create the process
/// without any associated console window. The process runs normally in the
/// background — only the visible window is suppressed.
///
/// ## Platform behaviour
/// - **Windows**: calls `.creation_flags(0x08000000)` on the Command builder.
///   `creation_flags` is defined by `std::os::windows::process::CommandExt`,
///   a Windows-only extension trait — hence the conditional `use` above.
/// - **Linux / macOS**: this function is a no-op. Processes on Unix have no
///   associated console window concept at the OS level.
///
/// ## Usage pattern
/// ```rust
/// let output = no_window(Command::new("mediainfo").arg("--version")).output();
/// ```
/// The function takes a `&mut Command` (mutable reference) and returns the same
/// reference, allowing it to be inserted into a builder chain without breaking
/// the fluent API style.
fn no_window(cmd: &mut Command) -> &mut Command {
    // `#[cfg(target_os = "windows")]` inside a function body: the line is only
    // compiled on Windows. On Linux/macOS, the function body is empty (no-op).
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW Win32 API flag
    cmd
}

// ── Data structure ────────────────────────────────────────────────────────────

/// Technical metadata extracted for one file.
///
/// All fields are `String` and default to `""` via `#[derive(Default)]`.
/// An empty string means "not applicable" or "mediainfo could not determine
/// this value" — both are normal conditions (e.g. an audio file has no resolution).
///
/// Callers should treat empty fields as absent rather than as errors.
///
/// ### Why `#[derive(Default)]`?
/// This auto-generates `FileMeta::default()` which initialises every String
/// field to `String::new()` (empty, no heap allocation). It enables the struct
/// update syntax: `FileMeta { name, file_type, ..Default::default() }` —
/// only the named fields need to be specified, all others default to empty.
///
/// ### Why `#[derive(Clone)]`?
/// Entries must be duplicated between the CSV and PDF generation pipelines
/// without transferring ownership. `clone()` makes an independent copy.
#[derive(Debug, Clone, Default)]
pub struct FileMeta {
    /// File name with extension. Example: `"IMG_0001.CR3"`.
    pub name:         String,

    /// File extension in uppercase. Example: `"MP4"`, `"CR3"`, `"WAV"`.
    pub file_type:    String,

    /// Human-readable file size. Example: `"2.34 GB"`, `"450 MB"`, `"128 KB"`.
    pub size_human:   String,

    /// Pixel resolution for images and video. Example: `"1920x1080"`, `"4096x3072"`.
    /// Empty for audio files and non-media files.
    pub resolution:   String,

    /// Normalised codec name. Example: `"H.264"`, `"ProRes"`, `"JPEG"`, `"FLAC"`.
    pub codec:        String,

    /// Playback duration as `HH:MM:SS` or `MM:SS`. Example: `"01:32:07"`, `"03:45"`.
    /// Empty for still images.
    pub duration:     String,

    /// Bit depth per channel. Example: `"10 bit"`, `"16 bit"`.
    pub bit_depth:    String,

    /// Chroma subsampling ratio (video only). Example: `"4:2:0"`, `"4:2:2"`.
    pub chroma:       String,

    /// Colour space / colour primaries. Example: `"BT.709"`, `"BT.2020"`, `"sRGB"`.
    pub color_space:  String,

    /// Audio sample rate. Example: `"48 kHz"`, `"96 kHz"`.
    pub sample_rate:  String,
}

// ── File type classification ──────────────────────────────────────────────────
//
// Extensions are compared after `.to_lowercase()` so "JPG" and "jpg" both match.
//
// `&[&str]` : a slice of static string references.
// The literals "jpg", "jpeg" etc. are `&'static str`: they live in the binary's
// read-only data section for the entire program lifetime. No heap allocation.

/// Still image formats, including RAW formats from major camera manufacturers.
const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "tiff", "tif", "webp", "bmp", "gif", "ico",
    "heic", "heif",     // Apple High Efficiency Image Format (iPhone, modern mirrorless)
    "raw",              // Generic RAW container
    "cr2", "cr3",       // Canon (CR3 = CRAW format since EOS R series)
    "nef",              // Nikon Electronic Format
    "arw",              // Sony Alpha RAW
    "dng",              // Adobe Digital Negative (open RAW container)
    "orf",              // Olympus RAW Format
    "rw2",              // Panasonic RAW
];

/// Video formats including broadcast and cinema containers.
const VIDEO_EXTS: &[&str] = &[
    "mp4", "mov", "mxf", "avi", "mkv", "m4v", "wmv", "flv", "webm",
    "m2ts", "mts", "ts",    // MPEG-2 Transport Stream (broadcast delivery)
    "mpg", "mpeg",
    "3gp", "ogv",
    "r3d",               // RED Cinema proprietary RAW video
    "braw",              // Blackmagic RAW (BMPCC cameras)
];

/// Audio-only formats including lossless and professional formats.
const AUDIO_EXTS: &[&str] = &[
    "mp3", "wav", "aac", "flac", "ogg", "m4a",
    "aif", "aiff",      // Audio Interchange File Format (Apple / professional audio)
    "opus", "wma",
    "alac",             // Apple Lossless Audio Codec
];

// ── Public extraction function ────────────────────────────────────────────────

/// Extracts metadata from `path` and returns a populated `FileMeta`.
///
/// This function **cannot fail** — it always returns a valid struct.
/// If `mediainfo` is unavailable or the format is unsupported, only the name,
/// type, and size fields are populated; all technical fields remain empty.
///
/// ## Dispatch logic
///
/// 1. Extract the file extension and convert to lowercase.
/// 2. Check which extension constant array it belongs to.
/// 3. Call the appropriate query function (`query_image`, `query_video`,
///    `query_audio`). Non-media files (Office docs, ZIPs…) only get
///    name, type, and size.
///
/// ## Interaction with rayon (parallelism)
///
/// This function is designed to be called in parallel from `copy_engine.rs`
/// via `rayon::par_iter()`. It modifies no global state, acquires no locks,
/// and performs only pure operations — all properties that make it safe in a
/// multi-threaded context (thread-safe by design).
pub fn extract(path: &Path) -> FileMeta {
    // `path.file_name()` : returns the last path component (the filename)
    // as an `OsStr`. We convert to String with `to_string_lossy` which handles
    // non-UTF-8 path components by replacing invalid bytes with '?'.
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default(); // Default for String = String::new()

    // `path.extension()` : returns the extension without the dot ("mov" from "clip.mov").
    // `.to_lowercase()` : normalises for case-insensitive comparison.
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    let size_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

    let file_type  = ext.to_uppercase();
    let size_human = format_size(size_bytes);

    // Struct update syntax: `..Default::default()` initialises all unlisted
    // fields with their default values (String::new() for Strings).
    // More readable than spelling out every empty field explicitly.
    let mut meta = FileMeta { name, file_type, size_human, ..Default::default() };

    // `.as_str()` : converts &String → &str for comparison with &[&str].
    // `.contains(&ext.as_str())` : checks if the extension is in the array.
    let is_image = IMAGE_EXTS.contains(&ext.as_str());
    let is_video = VIDEO_EXTS.contains(&ext.as_str());
    let is_audio = AUDIO_EXTS.contains(&ext.as_str());

    if is_image {
        query_image(path, &mut meta);
    } else if is_video {
        query_video(path, &mut meta);
    } else if is_audio {
        query_audio(path, &mut meta);
    }
    // Non-media files (PDFs, ZIPs, Office docs…): name/type/size only.
    // No mediainfo call is made — it would return empty output for these formats.

    meta
}

// ── mediainfo query helpers ───────────────────────────────────────────────────

/// Runs `mediainfo --Inform=<template> <path>` and returns the pipe-split fields.
///
/// Returns `None` if:
/// - `mediainfo` is not installed or not in `PATH`.
/// - The process exits with a non-zero code.
/// - The requested stream section does not exist in the file
///   (e.g. requesting a Video track from a JPEG returns empty output → `None`).
///
/// ### Why `Option` and not `Result`?
/// Absent metadata is a **normal** condition (an audio file has no video track).
/// Callers handle this naturally with `if let Some(fields) = …`.
/// Using `Result` would force naming an error type for a non-exceptional situation.
///
/// ### Call chain explained
/// ```text
/// Command::new("mediainfo")    // build the process
///   → no_window(…)            // suppress console window on Windows (no-op elsewhere)
///   .arg("--Inform=…")        // argument 1: the field template
///   .arg(path)                // argument 2: the file path (OsStr: handles spaces)
///   .output()                 // run, wait, capture stdout + stderr
///   .ok()?                    // Err (mediainfo not installed) → None, ? propagates
/// ```
fn run_inform(path: &Path, inform: &str) -> Option<Vec<String>> {
    // Build the command, passing it through no_window() to suppress the console
    // flash on Windows. no_window() takes &mut Command and returns &mut Command,
    // so it fits naturally into the builder chain.
    let out = no_window(
        Command::new("mediainfo")
            .arg(format!("--Inform={}", inform))
            // The path is passed as OsStr (via `.arg(path)`) rather than a String.
            // This handles paths with spaces, accents, or special characters
            // correctly without any manual escaping.
            .arg(path)
    )
    .output()
    .ok()?; // Err if mediainfo is absent or not executable → return None

    // `String::from_utf8_lossy` : converts &[u8] → Cow<str>, replacing any
    // invalid UTF-8 sequences with '?'. Safe even if mediainfo returns
    // non-UTF-8 bytes (very rare in practice).
    // `.trim()` : removes leading/trailing whitespace and newlines.
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() { return None; } // stream section absent from this file

    // `.split('|')` : splits the string on the pipe delimiter.
    // `.map(|f| f.trim().to_string())` : strips whitespace from each field.
    // `.collect()` : gathers into Vec<String>.
    Some(s.split('|').map(|f| f.trim().to_string()).collect())
}

/// Queries image metadata: resolution, codec, bit depth, colour space.
///
/// Two attempts are made:
/// 1. `Image` section — standard for JPEG, PNG, WebP, BMP.
/// 2. `Video` section — fallback for formats mediainfo classifies as video:
///    - RAW stills (CR2, NEF, ARW…) are typically in a Video section.
///    - Multi-page TIFF is sometimes classified as video.
///    - HEIC/HEIF may appear in either section depending on the mediainfo version.
///
/// ### `fields.get(0).map(|s| s.as_str()).unwrap_or("")`
/// - `fields.get(0)` : safe index access → `Option<&String>`
/// - `.map(|s| s.as_str())` : `Option<&String>` → `Option<&str>`
/// - `.unwrap_or("")` : default if field is absent
fn query_image(path: &Path, meta: &mut FileMeta) {
    if let Some(fields) = run_inform(path,
        "Image;%Width%|%Height%|%Format%|%BitDepth%|%ColorSpace%")
    {
        let w  = clean_number(fields.get(0).map(|s| s.as_str()).unwrap_or(""));
        let h  = clean_number(fields.get(1).map(|s| s.as_str()).unwrap_or(""));
        let f  = fields.get(2).cloned().unwrap_or_default();
        let b  = fields.get(3).cloned().unwrap_or_default();
        let cs = fields.get(4).cloned().unwrap_or_default();
        if !w.is_empty() && !h.is_empty() {
            meta.resolution  = format!("{}x{}", w, h);
            meta.codec       = friendly_codec(&f);
            if !b.is_empty()  { meta.bit_depth   = b; }
            if !cs.is_empty() { meta.color_space = cs; }
            return; // Image section provided valid data — no need for fallback
        }
    }
    // Fallback: Video section for RAW / HEIC / multi-page TIFF
    if let Some(fields) = run_inform(path,
        "Video;%Width%|%Height%|%Format%|%BitDepth%|%ColorSpace%")
    {
        let w  = clean_number(fields.get(0).map(|s| s.as_str()).unwrap_or(""));
        let h  = clean_number(fields.get(1).map(|s| s.as_str()).unwrap_or(""));
        let f  = fields.get(2).cloned().unwrap_or_default();
        let b  = fields.get(3).cloned().unwrap_or_default();
        let cs = fields.get(4).cloned().unwrap_or_default();
        if !w.is_empty() && !h.is_empty() {
            meta.resolution  = format!("{}x{}", w, h);
            meta.codec       = friendly_codec(&f);
            if !b.is_empty()  { meta.bit_depth   = b; }
            if !cs.is_empty() { meta.color_space = cs; }
        }
    }
}

/// Normalises chroma subsampling strings returned by mediainfo.
///
/// mediainfo can return several representations of the same ratio:
///   `"4:2:0"`, `"4:2:2"`, `"4:4:4"` — already correct
///   `"4:0:2"`, `"4:2"`               — malformed / abbreviated
///   `"YUV 4:2:2"` or `"4:2:2 (MPEG-2)"` — with extra text
///
/// This function extracts the first X:Y:Z pattern found in the string.
/// If only X:Y is found → interpreted as X:Y:0.
///
/// ### Byte-scanning approach
/// To avoid a regex dependency, we scan the raw bytes directly.
/// `bytes[i].is_ascii_digit()` : checks if a byte is an ASCII digit (0–9).
/// `b':'` : ASCII byte constant for ':' (value 58).
fn normalise_chroma(raw: &str) -> String {
    // Parse and normalise a chroma subsampling string from mediainfo.
    //
    // ## Why strip the colons?
    // The traditional notation "4:2:2" uses colons as separators. While human-readable,
    // these colons cause problems when the value is written to a CSV file because some
    // spreadsheet applications (Microsoft Excel, LibreOffice Calc) misinterpret
    // "4:2:2" as a time value (4 hours, 2 minutes, 2 seconds) and silently reformat
    // the cell as "04:02:02". This corrupts the data without any warning.
    //
    // The colon-free format ("420", "422", "444") is unambiguous, compact, and
    // already understood by most video professionals. It is also the format used
    // by codec documentation (e.g. "YUV420", "YUV422") and camera manufacturers.
    //
    // PDF reports are not affected by this issue (PDF cells are pure text), but
    // we use the same normalised value everywhere for consistency.
    //
    // ## Parsing strategy
    // We scan the raw string byte-by-byte looking for a digit:digit:digit (or
    // digit digit digit with space separators) pattern. We do not use a regex
    // crate to avoid adding a dependency for this single use case.
    //
    // `bytes[i].is_ascii_digit()` : true if the byte is '0'–'9' (ASCII 48–57).
    // `b':'` and `b' '` : ASCII byte literals for colon and space (58 and 32).
    // `bytes[i] - b'0'` : converts an ASCII digit byte to its numeric value (0–9).

    let s = raw.trim();
    let bytes = s.as_bytes();

    // Scan for a digit separator digit separator digit pattern.
    // `.saturating_sub(4)` : avoids underflow when len < 4 (returns 0 instead of panic).
    for i in 0..bytes.len().saturating_sub(4) {
        if bytes[i].is_ascii_digit() && i + 4 < bytes.len() {
            let a    = (bytes[i] - b'0') as u8;   // first component (always 4)
            let sep1 = bytes[i+1];                 // first separator (':' or ' ')
            if (sep1 == b':' || sep1 == b' ') && bytes[i+2].is_ascii_digit() {
                let b_ = (bytes[i+2] - b'0') as u8; // second component (0, 2, or 4)
                let sep2 = bytes[i+3];               // second separator
                if (sep2 == b':' || sep2 == b' ') && i+4 < bytes.len() && bytes[i+4].is_ascii_digit() {
                    let c_ = (bytes[i+4] - b'0') as u8; // third component (0, 2, or 4)
                    // Return colon-free format: "420", "422", "444", etc.
                    // Concatenate digits directly without separators.
                    return format!("{}{}{}", a, b_, c_);
                } else if sep2 != b':' && sep2 != b' ' {
                    // Only two components found — treat as 4:Y:0 (e.g. "4:2" → "420").
                    return format!("4{}0", b_);
                }
            }
        }
    }
    // Could not parse a known pattern — return the raw string unchanged.
    // This handles exotic values like "YUV" or empty strings gracefully.
    s.to_string()
}

/// Queries video metadata: resolution, codec, bit depth, chroma, colour space, duration.
///
/// Visual properties come from the `Video` stream section.
/// Duration comes from the `General` section (container level), because the Video
/// section duration can differ from actual playback duration when audio is longer
/// or shorter than the video track.
fn query_video(path: &Path, meta: &mut FileMeta) {
    if let Some(fields) = run_inform(path,
        "Video;%Width%|%Height%|%Format%|%BitDepth%|%ChromaSubsampling%|%ColorSpace%")
    {
        let w  = clean_number(fields.get(0).map(|s| s.as_str()).unwrap_or(""));
        let h  = clean_number(fields.get(1).map(|s| s.as_str()).unwrap_or(""));
        let f  = fields.get(2).cloned().unwrap_or_default();
        let b  = fields.get(3).cloned().unwrap_or_default();
        let ch = fields.get(4).cloned().unwrap_or_default();
        let cs = fields.get(5).cloned().unwrap_or_default();
        // Non-empty checks avoid writing empty strings to the struct fields
        if !w.is_empty() && !h.is_empty() { meta.resolution = format!("{}x{}", w, h); }
        if !f.is_empty()                  { meta.codec       = friendly_codec(&f); }
        if !b.is_empty()                  { meta.bit_depth   = format!("{} bit", b); }
        if !ch.is_empty()                 { meta.chroma      = normalise_chroma(&ch); }
        if !cs.is_empty()                 { meta.color_space = cs; }
    }
    // Duration from the General section (container level)
    if let Some(fields) = run_inform(path, "General;%Duration%") {
        if let Some(d) = fields.get(0) { meta.duration = format_duration(d); }
    }
}

/// Queries audio metadata: codec, bit depth, sample rate, and total duration.
///
/// All fields come from the `Audio` section except duration (from `General`).
fn query_audio(path: &Path, meta: &mut FileMeta) {
    if let Some(fields) = run_inform(path, "Audio;%Format%|%BitDepth%|%SamplingRate%") {
        let f  = fields.get(0).cloned().unwrap_or_default();
        let b  = fields.get(1).cloned().unwrap_or_default();
        let sr = fields.get(2).cloned().unwrap_or_default();
        if !f.is_empty()  { meta.codec       = friendly_codec(&f); }
        if !b.is_empty()  { meta.bit_depth   = format!("{} bit", b); }
        if !sr.is_empty() { meta.sample_rate = format_sample_rate(&sr); }
    }
    if let Some(fields) = run_inform(path, "General;%Duration%") {
        if let Some(d) = fields.get(0) { meta.duration = format_duration(d); }
    }
}

// ── CSV report generation ─────────────────────────────────────────────────────

/// Writes a `.csv` report file inside `dst_dir`.
///
/// ## File format
/// ```text
/// # Backup report
/// # Project,MyFilm              ← custom header lines (omitted if empty)
/// # Generated,2024-01-15 14:32
/// # Company,Studio Nord
/// # Contact,Alex Miller
/// #
/// Name,Type,Size,Resolution,…  ← column headers (active columns only)
/// clip001.mp4,MP4,2.34 GB,…    ← one row per file
/// ```
///
/// Comment lines starting with `#` are accepted by most CSV parsers
/// (Excel, LibreOffice Calc) as metadata / header lines.
///
/// ## Column selection
/// Only columns with `settings.col_*` set to `true` are written.
/// The column order is always the same regardless of which subset is active.
///
/// ## Audit note — CSV injection
/// Values containing commas, double-quotes, or newlines are correctly escaped
/// by `csv_escape()`. However, values starting with `=`, `+`, `-`, or `@` could
/// be interpreted as formulas by some spreadsheet applications. This is a known
/// limitation of the CSV format.
///
/// ## Function signature
/// `entries: &[(FileMeta, String, Option<bool>)]`
/// The tuple contains: (metadata, md5_hash, verify_status).
/// `Option<bool>` : `None` = copy-only mode, `Some(true)` = OK, `Some(false)` = ERROR.
pub fn write_csv(
    dst_dir:  &Path,
    src_name: &str,
    entries:  &[(FileMeta, String, String, Option<bool>)],
    // Each entry carries both hashes separately:
    //   .1 = md5 hash string  (empty string if MD5 was not computed)
    //   .2 = xxh3 hash string (empty string if XXH3 was not computed)
    // This avoids the previous single-hash design where one hash was silently dropped.
    settings: &Settings,
    gen_md5:  bool,  // true if MD5 was computed — adds "MD5" column header
    gen_xxh:  bool,  // true if XXH3 was computed — adds "XXH3" column header
) -> std::io::Result<()> {
    use std::io::Write; // Write trait: adds writeln!() support for files

    // Build the output path: "/destination/SourceName_report.csv"
    let path = dst_dir.join(format!("{}_report.csv", src_name));
    let mut f = std::fs::File::create(path)?; // create or overwrite

    write_custom_header_csv(&mut f, settings)?;

    // Detect whether at least one entry has a verification status.
    // `.any(|(_, _, s)| s.is_some())` : returns true on the first Some encountered.
    // Tuple destructuring in the closure: only the third field is used.
    let has_status = entries.iter().any(|(_, _, _, s)| s.is_some());

    // Build the header row dynamically.
    // We iterate the col_* flags to decide which columns to include.
    // The order defined here is always preserved regardless of the active subset.
    let mut headers: Vec<&str> = Vec::new();
    if settings.col_name        { headers.push("Name"); }
    if settings.col_type        { headers.push("Type"); }
    if settings.col_size        { headers.push("Size"); }
    if settings.col_resolution  { headers.push("Resolution"); }
    if settings.col_codec       { headers.push("Codec"); }
    if settings.col_duration    { headers.push("Duration"); }
    if settings.col_bit_depth   { headers.push("Bit Depth"); }
    if settings.col_chroma      { headers.push("Chroma Subsampling"); }
    if settings.col_color_space { headers.push("Color Space"); }
    if settings.col_sample_rate { headers.push("Sample Rate"); }
    // "Status" column appears between metadata columns and checksum — only in verify mode.
    if has_status { headers.push("Status"); }

    // Checksum column header — label depends on which algorithm(s) were computed.
    // If both MD5 and XXH3 are active, two separate columns are added.
    // If neither was computed (copy-only mode), no checksum column is added.
    let has_hash = gen_md5 || gen_xxh;
    if has_hash && gen_md5 && gen_xxh {
        headers.push("MD5");
        headers.push("XXH3");
    } else if has_hash && gen_md5 {
        headers.push("MD5");
    } else if has_hash && gen_xxh {
        headers.push("XXH3");
    }

    // `.join(",")` : concatenates elements with "," as separator.
    // Example: ["Name", "Type", "Size"] → "Name,Type,Size"
    writeln!(f, "{}", headers.join(","))?;

    // One row per file.
    // Each entry is (FileMeta, md5_hash, xxh3_hash, verify_status).
    for (meta, md5, xxh3, ok) in entries {
        let mut cols: Vec<String> = Vec::new();
        if settings.col_name        { cols.push(csv_escape(&meta.name)); }
        if settings.col_type        { cols.push(meta.file_type.clone()); }
        if settings.col_size        { cols.push(meta.size_human.clone()); }
        if settings.col_resolution  { cols.push(meta.resolution.clone()); }
        if settings.col_codec       { cols.push(meta.codec.clone()); }
        if settings.col_duration    { cols.push(meta.duration.clone()); }
        if settings.col_bit_depth   { cols.push(meta.bit_depth.clone()); }
        if settings.col_chroma      { cols.push(meta.chroma.clone()); }
        if settings.col_color_space { cols.push(meta.color_space.clone()); }
        if settings.col_sample_rate { cols.push(meta.sample_rate.clone()); }
        if has_status {
            // Map Option<bool> → human-readable status string.
            cols.push(match ok {
                Some(true)  => "OK".to_string(),
                Some(false) => "ERROR".to_string(),
                None        => String::new(),
            });
        }
        // Emit checksum column(s) — each hash is now a separate String field.
        // Empty strings are written when a hash was not computed, ensuring the
        // column count always matches the header row (no misaligned columns).
        if gen_md5 && gen_xxh {
            cols.push(md5.clone());   // MD5 column — always populated when gen_md5
            cols.push(xxh3.clone());  // XXH3 column — always populated when gen_xxh
        } else if gen_md5 {
            cols.push(md5.clone());
        } else if gen_xxh {
            cols.push(xxh3.clone());
        }
        writeln!(f, "{}", cols.join(","))?;
    }
    Ok(())
}

/// Writes comment-style header lines for non-empty custom fields.
/// A blank `#` line is appended as a visual separator before the column headers.
fn write_custom_header_csv(f: &mut std::fs::File, s: &Settings) -> std::io::Result<()> {
    use std::io::Write;

    // `Local::now()` : current local date and time.
    // `.format(…)` : ISO 8601 readable format. Example: "2024-01-15  14:32".
    let now = Local::now().format("%Y-%m-%d  %H:%M").to_string();
    writeln!(f, "# Backup report")?;
    if !s.project_title.is_empty() {
        writeln!(f, "# Project,{}", csv_escape(&s.project_title))?;
    }
    writeln!(f, "# Generated,{}", now)?;

    // Optional contact fields — written only if non-empty
    let mut wrote_any = false;
    if !s.company.is_empty()      { writeln!(f, "# Company,{}", csv_escape(&s.company))?;      wrote_any = true; }
    if !s.contact_name.is_empty() { writeln!(f, "# Contact,{}", csv_escape(&s.contact_name))?; wrote_any = true; }
    if !s.email.is_empty()        { writeln!(f, "# Email,{}", csv_escape(&s.email))?;           wrote_any = true; }
    if !s.phone.is_empty()        { writeln!(f, "# Phone,{}", csv_escape(&s.phone))?;           wrote_any = true; }
    // `let _ = wrote_any` suppresses the "unused variable" compiler warning for
    // a flag that is only used inside conditional branches.
    let _ = wrote_any;

    writeln!(f, "#")?; // blank separator line before column headers
    Ok(())
}

// ── Utility functions ─────────────────────────────────────────────────────────

/// Strips all non-ASCII-digit characters from a string.
///
/// mediainfo may return numbers with locale-specific thousand separators:
/// - `"1 920"` (French locale, regular space U+0020)
/// - `"1\u{202F}920"` (narrow no-break space U+202F)
///
/// This function removes any separator, returning `"1920"` in both cases.
///
/// `.chars()` : iterates over Unicode characters (not raw bytes).
/// `.filter(|c| c.is_ascii_digit())` : keeps only ASCII digits 0–9.
/// `.collect()` : gathers the remaining characters back into a String.
fn clean_number(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Formats a byte count as a human-readable string using binary prefixes (1 KB = 1024 bytes).
///
/// - `>= 1 GB` → `"N.NN GB"` (2 decimal places)
/// - `>= 1 MB` → `"N.N MB"` (1 decimal place)
/// - `>= 1 KB` → `"N KB"` (0 decimal places)
/// - `< 1 KB`  → `"N B"`
///
/// `pub` because it is also used by `pdf_report.rs` for the size column.
///
/// ### Digit separator
/// `const KB: u64 = 1_024` — the `_` is a Rust digit separator (like a comma in
/// English or a space in French notation). It improves readability with no effect
/// on the value. `1_024 == 1024`.
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1_024;
    const MB: u64 = 1_024 * KB;
    const GB: u64 = 1_024 * MB;
    if bytes >= GB      { format!("{:.2} GB", bytes as f64 / GB as f64) }
    else if bytes >= MB { format!("{:.1} MB", bytes as f64 / MB as f64) }
    else if bytes >= KB { format!("{:.0} KB", bytes as f64 / KB as f64) }
    else                { format!("{} B", bytes) }
}

/// Converts a duration in milliseconds (mediainfo format) to `HH:MM:SS` or `MM:SS`.
///
/// mediainfo always returns duration as a floating-point number of milliseconds.
/// Examples:
/// - `"5400000"` → `"01:30:00"` (1 hour 30 minutes)
/// - `"185000"`  → `"03:05"` (3 minutes 5 seconds)
/// - `""` or unparseable value → `""` (empty string)
///
/// ### `s.parse::<f64>()`
/// The `::<f64>` turbofish syntax specifies the target type for the generic `parse()`
/// method. Returns `Result<f64, ParseFloatError>`.
/// `if let Ok(ms)` : only perform the conversion if parsing succeeded.
fn format_duration(s: &str) -> String {
    if let Ok(ms) = s.parse::<f64>() {
        let total_sec = (ms / 1000.0) as u64; // ms → seconds, truncated
        let h   = total_sec / 3600;
        let m   = (total_sec % 3600) / 60;    // `%` : modulo (remainder after division)
        let sec = total_sec % 60;
        if h > 0 { format!("{:02}:{:02}:{:02}", h, m, sec) }
        else     { format!("{:02}:{:02}", m, sec) }
    } else {
        String::new() // unparseable → empty field
    }
}

/// Formats a sample rate in Hz (mediainfo format) as a readable kHz string.
///
/// Standard frequencies are mapped to canonical strings.
/// Non-standard frequencies are converted with one decimal place.
///
/// The frequency is first cleaned with `clean_number` to handle locale-specific
/// thousand separators (e.g. `"48 000"` → `"48000"`).
fn format_sample_rate(s: &str) -> String {
    let clean: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    // `match` on string slices: each arm matches an exact string value.
    // `=>` : the match arm operator (binds the pattern to its result expression).
    match clean.as_str() {
        "44100"  => "44.1 kHz".into(), // CD quality (consumer)
        "48000"  => "48 kHz".into(),   // Broadcast / film standard
        "88200"  => "88.2 kHz".into(), // Double CD (rare)
        "96000"  => "96 kHz".into(),   // High resolution
        "176400" => "176.4 kHz".into(), // Quadruple CD (rare)
        "192000" => "192 kHz".into(),  // Maximum high resolution
        other => {
            // Non-standard frequency: convert numerically
            if let Ok(hz) = other.parse::<f64>() {
                format!("{:.1} kHz", hz / 1000.0)
            } else {
                s.to_string() // fallback: return the raw string
            }
        }
    }
}

/// Translates mediainfo's internal format identifiers to user-friendly names.
///
/// mediainfo uses its own naming conventions which often differ from the commercial
/// names users recognise. Unknown identifiers are returned as-is (uppercased)
/// rather than being silently dropped.
///
/// ### `.to_uppercase().as_str()`
/// Normalising to uppercase handles case variations across different mediainfo
/// versions. `.as_str()` returns a `&str` from the temporary String for `match`.
fn friendly_codec(raw: &str) -> String {
    match raw.to_uppercase().as_str() {
        // Video codecs
        "AVC"           => "H.264".into(),    // ITU-T H.264 / MPEG-4 Part 10 (most common)
        "HEVC"          => "H.265".into(),    // ITU-T H.265 / MPEG-H Part 2 (4K streaming)
        "AV1"           => "AV1".into(),      // AOMedia Video 1 (open source, YouTube/Netflix)
        "VP9"           => "VP9".into(),      // Google VP9
        "VP8"           => "VP8".into(),      // Google VP8 (WebRTC)
        "MPEG-4 VISUAL" => "MPEG-4".into(),   // MPEG-4 Part 2 (DivX/Xvid, early 2000s)
        "MPEG VIDEO"    => "MPEG-2".into(),   // DVD and broadcast SD delivery
        "PRORES"        => "ProRes".into(),   // Apple ProRes (post-production, editing)
        "DNXHD"         => "DNxHD".into(),    // Avid DNxHD / DNxHR (post-production)
        // Image formats
        "JPEG"          => "JPEG".into(),
        "JPEG 2000"     => "JPEG 2000".into(),
        "PNG"           => "PNG".into(),
        "TIFF"          => "TIFF".into(),
        "BMP"           => "BMP".into(),
        "GIF"           => "GIF".into(),
        "WEBP"          => "WebP".into(),
        // Audio codecs
        "AAC LC"        => "AAC".into(),      // Advanced Audio Coding, Low Complexity profile
        "AAC"           => "AAC".into(),
        "MP3"           => "MP3".into(),      // MPEG Audio Layer III
        "MPEG AUDIO"    => "MP3".into(),
        "FLAC"          => "FLAC".into(),     // Free Lossless Audio Codec
        "PCM"           => "PCM".into(),      // Pulse-Code Modulation (uncompressed)
        "ALAC"          => "ALAC".into(),     // Apple Lossless Audio Codec
        "OPUS"          => "Opus".into(),     // Opus codec (WebRTC, Discord)
        "VORBIS"        => "Vorbis".into(),   // Vorbis codec (OGG container)
        // Unknown: return as-is
        other           => other.to_string(),
    }
}

/// Escapes a field value for RFC 4180-compliant CSV output.
///
/// A value must be wrapped in double quotes if it contains:
/// - A comma (column separator)
/// - A double quote (which must also be doubled: `"` → `""`)
/// - A newline (which would start a new CSV row)
///
/// Examples:
/// - `"Studio Nord"` → `"Studio Nord"` (no special characters, no quoting)
/// - `"Studio, Nord"` → `"\"Studio, Nord\""` (comma → needs quoting)
/// - `"Title: \"Film\""` → `"\"Title: \"\"Film\"\""` (quotes → doubled)
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        // `.replace('"', "\"\"")` : each " is replaced by ""
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
