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
//! `sidecar::run_with_timeout()` applies the `CREATE_NO_WINDOW` flag to
//! suppress it, and kills a mediainfo call that hangs.

use std::path::{Path, PathBuf};
use crate::settings::Settings;
use chrono::Local;

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

    /// Gamut (colour primaries). Example: `"BT.709"`, `"S-Gamut3.Cine"`, `"AWG4"`.
    /// Shown in the "Gamut" column (setting `col_color_space`).
    pub color_space:  String,

    /// Transfer function / log curve. Example: `"BT.709"`, `"PQ"`, `"S-Log3"`, `"LogC4"`.
    pub transfer:     String,

    /// Camera (and recorder, for external recorders). Example: `"ARRI ALEXA 35"`,
    /// `"Sony (Atomos Shogun7)"`.
    pub camera:       String,

    /// ISO / exposure index as recorded by the camera. Example: `"800"`.
    pub iso:          String,

    /// White balance as recorded by the camera. Example: `"5600K"`.
    pub white_balance: String,

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

// Single source of truth for file-type detection: metadata extraction and
// the PDF / HTML thumbnails all use these lists.

/// Image-sequence frame formats (one file per frame): no per-file thumbnail
/// in the reports — a 20 000-frame ARRIRAW or DPX folder would otherwise
/// spawn ffmpeg 20 000 times.
pub(crate) const SEQUENCE_EXTS: &[&str] = &["dpx", "exr", "ari"];

/// Still image formats, including RAW formats from major camera manufacturers.
pub(crate) const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "tiff", "tif", "webp", "bmp", "gif", "ico",
    "heic", "heif",     // Apple High Efficiency Image Format (iPhone, modern mirrorless)
    "raw",              // Generic RAW container
    "cr2", "cr3",       // Canon (CR3 = CRAW format since EOS R series)
    "nef",              // Nikon Electronic Format
    "arw",              // Sony Alpha RAW
    "dng",              // Adobe Digital Negative (open RAW container)
    "orf",              // Olympus RAW Format
    "rw2",              // Panasonic RAW
    "raf",              // Fujifilm RAW
    "dpx", "exr",       // VFX / scan image sequences
];

/// Video formats including broadcast and cinema containers.
pub(crate) const VIDEO_EXTS: &[&str] = &[
    "mp4", "mov", "mxf", "avi", "mkv", "m4v", "wmv", "flv", "webm",
    "m2ts", "mts", "ts",    // MPEG-2 Transport Stream (broadcast delivery)
    "mpg", "mpeg",
    "3gp", "ogv",
    "r3d",               // RED Cinema proprietary RAW video
    "braw",              // Blackmagic RAW (BMPCC cameras)
    "crm",               // Canon Cinema RAW Light
    "nev",               // Nikon N-RAW
    "ari", "arx",        // ARRIRAW frame / ARRIRAW HDE
];

/// Audio-only formats including lossless and professional formats.
pub(crate) const AUDIO_EXTS: &[&str] = &[
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
    let mut cmd = crate::sidecar::mediainfo_cmd();
    cmd.arg(format!("--Inform={}", inform)).arg(path);
    let out = crate::sidecar::run_with_timeout(&mut cmd, crate::sidecar::MEDIAINFO_TIMEOUT).ok()?;

    // `String::from_utf8_lossy` : converts &[u8] → Cow<str>, replacing any
    // invalid UTF-8 sequences with '?'. Safe even if mediainfo returns
    // non-UTF-8 bytes (very rare in practice).
    // `.trim()` : removes leading/trailing whitespace and newlines.
    // Templates end with `\n` so that a file with several tracks of the same
    // kind (two video tracks, eight mono audio tracks…) yields one line per
    // track instead of all tracks glued together. Only the first track is used.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let s = stdout.lines().map(str::trim).find(|l| !l.is_empty())?.to_string();
    // A line made only of separators means the section exists but every
    // requested field is empty.
    if s.chars().all(|c| c == '|') { return None; }

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
        "Image;%Width%|%Height%|%Format%|%BitDepth%|%colour_primaries%\\n")
    {
        let w  = clean_number(fields.get(0).map(|s| s.as_str()).unwrap_or(""));
        let h  = clean_number(fields.get(1).map(|s| s.as_str()).unwrap_or(""));
        let f  = fields.get(2).cloned().unwrap_or_default();
        let b  = fields.get(3).cloned().unwrap_or_default();
        let cs = fields.get(4).cloned().unwrap_or_default();
        if !w.is_empty() && !h.is_empty() {
            meta.resolution  = format!("{}x{}", w, h);
            meta.codec       = friendly_codec(&f);
            if !b.is_empty()  { meta.bit_depth   = format!("{} bit", first_number(&b)); }
            if !cs.is_empty() { meta.color_space = cs; }
            return; // Image section provided valid data — no need for fallback
        }
    }
    // Fallback: Video section for RAW / HEIC / multi-page TIFF
    if let Some(fields) = run_inform(path,
        "Video;%Width%|%Height%|%Format%|%BitDepth%|%colour_primaries%\\n")
    {
        let w  = clean_number(fields.get(0).map(|s| s.as_str()).unwrap_or(""));
        let h  = clean_number(fields.get(1).map(|s| s.as_str()).unwrap_or(""));
        let f  = fields.get(2).cloned().unwrap_or_default();
        let b  = fields.get(3).cloned().unwrap_or_default();
        let cs = fields.get(4).cloned().unwrap_or_default();
        if !w.is_empty() && !h.is_empty() {
            meta.resolution  = format!("{}x{}", w, h);
            meta.codec       = friendly_codec(&f);
            if !b.is_empty()  { meta.bit_depth   = format!("{} bit", first_number(&b)); }
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
///
/// ## Codec naming
/// The bare `%Format%` is too coarse for professional formats ("ProRes" for
/// every ProRes flavour, "VC-3" for DNxHD/DNxHR, "MPEG Video" for XDCAM), so
/// `video_codec_name()` combines it with the profile, the commercial name and
/// the codec FourCC — see that function.
///
/// ## Gaps filled from the codec specification
/// mediainfo does not parse ProRes frame headers deeply enough to report a
/// bit depth, and it cannot decode ProRes RAW at all (no bit depth, chroma or
/// colour space). Where the value is fixed by the format specification, it is
/// filled in by `spec_bit_depth()`; RAW formats get explicit "RAW" markers in
/// the chroma / colour-space columns instead of blank cells, because Bayer
/// sensor data has no chroma subsampling and no encoded colour space.
fn query_video(path: &Path, meta: &mut FileMeta) {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if let Some(fields) = run_inform(path,
        "Video;%Width%|%Height%|%Format%|%Format_Profile%|%Format_Commercial_IfAny%|%CodecID%|%BitDepth%|%ChromaSubsampling%|%ColorSpace%|%colour_primaries%|%transfer_characteristics%\\n")
    {
        let field = |i: usize| fields.get(i).map(|s| s.as_str()).unwrap_or("");
        let w        = clean_number(field(0));
        let h        = clean_number(field(1));
        let format   = field(2);
        let profile  = field(3);
        let comm     = field(4);
        let codec_id = field(5);
        let b        = field(6);
        let ch       = field(7);
        let cs       = field(8);
        let prim     = field(9);
        let transfer = field(10);

        let codec = video_codec_name(format, profile, comm, codec_id);
        let raw   = is_raw_video(&codec, codec_id, &ext);

        // Non-empty checks avoid writing empty strings to the struct fields
        if !w.is_empty() && !h.is_empty() { meta.resolution = format!("{}x{}", w, h); }
        if !b.is_empty() {
            meta.bit_depth = format!("{} bit", first_number(b));
        } else if let Some(depth) = spec_bit_depth(&codec, profile) {
            meta.bit_depth = format!("{} bit", depth);
        }
        if !ch.is_empty()  { meta.chroma = normalise_chroma(ch); }
        else if raw        { meta.chroma = "RAW".into(); }
        let _ = cs; // "YUV" / "RGB" is a pixel encoding, not a gamut — not reported
        meta.color_space = mediainfo_colour_value(prim);
        meta.transfer    = mediainfo_colour_value(transfer);
        meta.codec = codec;
    }
    // Duration from the General section (container level)
    if let Some(fields) = run_inform(path, "General;%Duration%") {
        if let Some(d) = fields.get(0) { meta.duration = format_duration(d); }
    }
    query_camera(path, &ext, meta);
}

/// Formats a colour value from mediainfo. Vendor-private MXF labels come out
/// of mediainfo as 16 hex digits ("0E06040101010605"); they are prefixed with
/// "UL" so they read as a code, not as a name. `query_camera()` replaces the
/// labels it can identify.
fn mediainfo_colour_value(v: &str) -> String {
    let v = v.trim();
    if v.len() == 16 && v.chars().all(|c| c.is_ascii_hexdigit()) {
        format!("UL {}", v.to_uppercase())
    } else {
        v.to_string()
    }
}

/// Fills gamut, log curve, camera, ISO and white balance from camera metadata.
///
/// Sources are applied from least to most trusted, each one overriding the
/// previous values it provides (see `camera_meta` for the rationale):
/// 1. mediainfo JSON — ARRI camera metadata, RDD 18 acquisition metadata,
///    QuickTime `mdta` keys (Atomos / Apple ProRes RAW).
/// 2. MXF picture-descriptor labels (vendor-private gamut / transfer).
/// 3. Sony XML sidecar.
/// 4. R3D header (mediainfo reads nothing from R3D files).
///
/// The RDD 18 `ColorPrimaries` / `TransferCharacteristics` items are ignored:
/// on the Sony FS5 sample they say BT.709 while the clip is S-Gamut3.Cine / S-Log3.
fn query_camera(path: &Path, ext: &str, meta: &mut FileMeta) {
    if let Some(json) = run_json(path) {
        apply_mediainfo_json(&json, meta);
    }
    if ext == "mxf" {
        let m = crate::camera_meta::read_mxf(path);
        set_label(&mut meta.color_space, m.gamut);
        set_label(&mut meta.transfer,    m.transfer);
        if let Some(m) = crate::camera_meta::read_sony_xml(path) {
            set_label(&mut meta.color_space, m.gamut);
            set_label(&mut meta.transfer,    m.transfer);
            if !m.camera.is_empty()   { meta.camera      = m.camera; }
        }
    }
    if ext == "r3d" {
        if let Some(m) = crate::camera_meta::read_r3d(path) {
            if !m.camera.is_empty()        { meta.camera        = m.camera; }
            if !m.iso.is_empty()           { meta.iso           = m.iso; }
            if !m.white_balance.is_empty() { meta.white_balance = m.white_balance; }
            if !m.codec.is_empty()         { meta.codec         = m.codec; }
            if meta.resolution.is_empty()  { meta.resolution    = m.resolution; }
        }
        if meta.chroma.is_empty() { meta.chroma = "RAW".into(); }
    }
    // ProRes RAW stores sensor data with no gamut / log tag: the log
    // conversion is chosen at debayer time (FCP "RAW to Log", Resolve RAW
    // settings). Verified on an Atomos Shogun 7 + FS5 sample: neither the
    // sample description nor the frame header carry one.
    if meta.codec.starts_with("ProRes RAW") {
        if meta.color_space.is_empty() { meta.color_space = "Not recorded".into(); }
        if meta.transfer.is_empty()    { meta.transfer    = "Not recorded".into(); }
    }
}

/// Replaces a gamut / transfer value with one read from the file, except that
/// an unknown MXF label (shown as a raw "UL …" code) never overwrites a name
/// already found elsewhere (mediainfo JSON, ARRI LUT metadata…).
fn set_label(cur: &mut String, new: String) {
    if new.is_empty() { return; }
    if new.starts_with("UL ") && !cur.is_empty() && !cur.starts_with("UL ") { return; }
    *cur = new;
}

/// Runs `mediainfo --Output=JSON` and parses the result.
fn run_json(path: &Path) -> Option<serde_json::Value> {
    let mut cmd = crate::sidecar::mediainfo_cmd();
    cmd.arg("--Output=JSON").arg(path);
    let out = crate::sidecar::run_with_timeout(&mut cmd, crate::sidecar::MEDIAINFO_TIMEOUT).ok()?;
    serde_json::from_slice(&out.stdout).ok()
}

/// Camera vendors whose MXF `Encoded_Application` names the camera itself
/// ("Canon" / "EOS C80", "ARRI" / "ALEXA 35"). Sony writes its recording
/// library ("Mem") there and post software writes its own name, so neither is used.
const CAMERA_APP_VENDORS: &[&str] = &["Canon", "ARRI", "Panasonic"];

fn apply_mediainfo_json(json: &serde_json::Value, meta: &mut FileMeta) {
    let Some(tracks) = json["media"]["track"].as_array() else { return; };
    let s = |v: &serde_json::Value, k: &str| v[k].as_str().unwrap_or("").trim().to_string();

    for t in tracks {
        let kind  = t["@type"].as_str().unwrap_or("");
        let extra = &t["extra"];
        match (kind, t["Format"].as_str().unwrap_or("")) {
            ("General", _) => {
                let company = s(t, "Encoded_Application_CompanyName");
                if CAMERA_APP_VENDORS.iter().any(|v| v.eq_ignore_ascii_case(&company)) {
                    meta.camera = crate::camera_meta::join_make_model(
                        &company, &s(t, "Encoded_Application_Name"));
                }
                // QuickTime keys written by Atomos recorders (and Apple ProRes RAW tools).
                let make     = s(extra, "com_apple_proapps_manufacturer");
                let rec_make = s(extra, "com_apple_proapps_image_TIFF_Make");
                let rec_mod  = s(extra, "com_apple_proapps_image_TIFF_Model");
                let recorder = crate::camera_meta::join_make_model(&rec_make, &rec_mod);
                if !make.is_empty() {
                    meta.camera = if recorder.is_empty() || recorder.eq_ignore_ascii_case(&make) {
                        make
                    } else {
                        format!("{} ({})", make, recorder)
                    };
                }
                let iso = s(extra, "org_smpte_rdd18_camera_isosensitivity");
                if !iso.is_empty() { meta.iso = iso; }
                let wb = s(extra, "org_smpte_rdd18_camera_whitebalance");
                if !wb.is_empty() { meta.white_balance = format_kelvin(&wb); }
            }
            ("Other", "Acquisition Metadata") => {
                let mut iso = s(extra, "ISOSensitivity_FirstFrame");
                if iso.is_empty() { iso = s(extra, "ExposureIndexofPhotoMeter_FirstFrame"); }
                if !iso.is_empty() { meta.iso = iso; }
                let wb = s(extra, "WhiteBalance_FirstFrame");
                if !wb.is_empty() { meta.white_balance = format_kelvin(&wb); }
            }
            ("Other", "ARRI Camera Metadata") => {
                let model = s(extra, "CameraModel");
                if !model.is_empty() { meta.camera = model; }
                // Colour space the camera's own LUT expects as input = the
                // encoding of the recorded image (confirmed on ALEXA 35 ProRes,
                // where it matches the MXF descriptor labels).
                let p = s(extra, "Lut3D_0_SourceColorSpace_Primaries");
                if !p.is_empty() { meta.color_space = p; }
                let c = s(extra, "Lut3D_0_SourceColorSpace_TransferCurve");
                if !c.is_empty() { meta.transfer = c; }
            }
            _ => {}
        }
    }
}

/// "5500" / "5500K" / "5500 K" → "5500K".
fn format_kelvin(v: &str) -> String {
    let n = first_number(v);
    if n.is_empty() { v.to_string() } else { format!("{}K", n) }
}

/// Builds the codec label shown in reports from several mediainfo fields.
///
/// - ProRes RAW is identified by its FourCC (`aprn` = ProRes RAW,
///   `aprh` = ProRes RAW HQ): depending on the mediainfo version, `%Format%`
///   only says "ProRes" for these files.
/// - ProRes gets its profile appended: "ProRes 422 HQ", "ProRes 4444 XQ"…
/// - When mediainfo knows a commercial name ("DNxHR HQX", "XDCAM HD422",
///   "AVC-Intra 100", "XAVC"…) it is more meaningful than the format and wins.
/// - Otherwise the format goes through `friendly_codec()` ("AVC" → "H.264").
fn video_codec_name(format: &str, profile: &str, commercial: &str, codec_id: &str) -> String {
    match codec_id.trim().to_lowercase().as_str() {
        "aprn" => return "ProRes RAW".into(),
        "aprh" => return "ProRes RAW HQ".into(),
        _ => {}
    }
    let up = format.to_uppercase();
    if up.starts_with("PRORES") {
        // "ProRes" or, on newer mediainfo versions, possibly "ProRes RAW".
        // `get` instead of slicing: never panics, even if to_uppercase()
        // changed the byte length of a non-ASCII format string.
        let base = format!("ProRes{}", format.get("ProRes".len()..).unwrap_or(""));
        let p = profile.trim();
        if p.is_empty() || base.to_uppercase().contains(&p.to_uppercase()) {
            return base;
        }
        return format!("{} {}", base, p);
    }
    let c = commercial.trim();
    if !c.is_empty() && !c.eq_ignore_ascii_case(format) {
        return c.to_string();
    }
    if up == "VC-3" { return "DNxHD".into(); }
    friendly_codec(format)
}

/// True for camera RAW video codecs (Bayer sensor data, no YUV/RGB encoding).
fn is_raw_video(codec: &str, codec_id: &str, ext: &str) -> bool {
    let c = codec.to_uppercase();
    c.contains("RAW")            // ProRes RAW, ARRIRAW, Blackmagic RAW, Cinema RAW Light…
        || c.contains("X-OCN")   // Sony Venice / Burano
        || c.contains("REDCODE") || c == "R3D"
        || matches!(codec_id.trim().to_lowercase().as_str(), "aprn" | "aprh")
        || matches!(ext, "r3d" | "braw" | "crm" | "nev" | "ari" | "arx")
}

/// Bit depth fixed by the codec specification, for codecs where mediainfo
/// leaves `%BitDepth%` empty.
///
/// - ProRes RAW / RAW HQ: 12-bit log-encoded sensor data (Apple white paper).
/// - ProRes 4444 / 4444 XQ: 12-bit.
/// - ProRes 422 family (Proxy, LT, 422, HQ): 10-bit.
fn spec_bit_depth(codec: &str, profile: &str) -> Option<u32> {
    let c = codec.to_uppercase();
    if !c.starts_with("PRORES") { return None; }
    if c.contains("RAW") || c.contains("4444") || profile.contains("4444") {
        Some(12)
    } else {
        Some(10)
    }
}

/// Queries audio metadata: codec, bit depth, sample rate, and total duration.
///
/// All fields come from the `Audio` section except duration (from `General`).
fn query_audio(path: &Path, meta: &mut FileMeta) {
    if let Some(fields) = run_inform(path, "Audio;%Format%|%BitDepth%|%SamplingRate%\\n") {
        let f  = fields.get(0).cloned().unwrap_or_default();
        let b  = fields.get(1).cloned().unwrap_or_default();
        let sr = fields.get(2).cloned().unwrap_or_default();
        if !f.is_empty()  { meta.codec       = friendly_codec(&f); }
        if !b.is_empty()  { meta.bit_depth   = format!("{} bit", first_number(&b)); }
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
/// Strips HTML tags and converts block-level elements to newlines.
/// Used to render the WYSIWYG comment as plain text in CSV and PDF headers.
fn html_to_plain(html: &str) -> String {
    let mut result = String::new();
    let mut in_tag = false;
    let mut tag_buf = String::new();
    for ch in html.chars() {
        match ch {
            '<' => { in_tag = true; tag_buf.clear(); }
            '>' => {
                let t = tag_buf.trim().to_lowercase();
                if t == "br" || t == "br/" || t.starts_with("/p") || t.starts_with("/div") {
                    if !result.ends_with('\n') { result.push('\n'); }
                }
                in_tag = false;
            }
            _ if in_tag => { tag_buf.push(ch); }
            _ => { result.push(ch); }
        }
    }
    result
        .replace("&amp;",  "&")
        .replace("&lt;",   "<")
        .replace("&gt;",   ">")
        .replace("&nbsp;", " ")
        .replace("&#39;",  "'")
        .replace("&quot;", "\"")
        .trim()
        .to_string()
}

pub fn write_csv(
    dst_dir:         &Path,
    report_name:     &str,
    timestamp:       &str,
    src_path:        &Path,
    src_total_bytes: u64,
    destinations:    &[PathBuf],
    entries:         &[(FileMeta, String, Option<bool>)],
    settings:        &Settings,
    hash_col:        &str,
    comment:         &str,
    location:        &str,
) -> std::io::Result<()> {
    use std::io::Write;

    let path = if timestamp.is_empty() {
        dst_dir.join(format!("{}.csv", report_name))
    } else {
        dst_dir.join(format!("{}_{}.csv", report_name, timestamp))
    };
    let mut f = std::fs::File::create(path)?;

    write_custom_header_csv(&mut f, settings, src_path, src_total_bytes, destinations, comment, location)?;

    let has_status = entries.iter().any(|(_, _, s)| s.is_some());
    let has_hash   = !hash_col.is_empty();

    let mut headers: Vec<&str> = Vec::new();
    if settings.col_name        { headers.push("Name"); }
    if settings.col_type        { headers.push("Type"); }
    if settings.col_size        { headers.push("Size"); }
    if settings.col_resolution  { headers.push("Resolution"); }
    if settings.col_codec       { headers.push("Codec"); }
    if settings.col_duration    { headers.push("Duration"); }
    if settings.col_bit_depth   { headers.push("Bit Depth"); }
    if settings.col_chroma      { headers.push("Chroma Subsampling"); }
    if settings.col_color_space { headers.push("Gamut"); }
    if settings.col_transfer    { headers.push("Gamma / Log"); }
    if settings.col_camera      { headers.push("Camera"); }
    if settings.col_iso         { headers.push("ISO"); }
    if settings.col_white_balance { headers.push("White Balance"); }
    if settings.col_sample_rate { headers.push("Sample Rate"); }
    if has_status               { headers.push("Status"); }
    if has_hash                 { headers.push(hash_col); }

    writeln!(f, "{}", headers.join(","))?;

    for (meta, hash, ok) in entries {
        let mut cols: Vec<String> = Vec::new();
        if settings.col_name        { cols.push(csv_escape(&meta.name)); }
        if settings.col_type        { cols.push(meta.file_type.clone()); }
        if settings.col_size        { cols.push(meta.size_human.clone()); }
        if settings.col_resolution  { cols.push(meta.resolution.clone()); }
        if settings.col_codec       { cols.push(csv_escape(&meta.codec)); }
        if settings.col_duration    { cols.push(meta.duration.clone()); }
        if settings.col_bit_depth   { cols.push(meta.bit_depth.clone()); }
        if settings.col_chroma      { cols.push(meta.chroma.clone()); }
        if settings.col_color_space { cols.push(csv_escape(&meta.color_space)); }
        if settings.col_transfer    { cols.push(csv_escape(&meta.transfer)); }
        if settings.col_camera      { cols.push(csv_escape(&meta.camera)); }
        if settings.col_iso         { cols.push(csv_escape(&meta.iso)); }
        if settings.col_white_balance { cols.push(csv_escape(&meta.white_balance)); }
        if settings.col_sample_rate { cols.push(meta.sample_rate.clone()); }
        if has_status {
            cols.push(match ok {
                Some(true)  => "OK".to_string(),
                Some(false) => "ERROR".to_string(),
                None        => String::new(),
            });
        }
        if has_hash { cols.push(hash.clone()); }
        writeln!(f, "{}", cols.join(","))?;
    }
    Ok(())
}

fn write_custom_header_csv(
    f:               &mut std::fs::File,
    s:               &Settings,
    src_path:        &Path,
    src_total_bytes: u64,
    destinations:    &[PathBuf],
    comment:         &str,
    location:        &str,
) -> std::io::Result<()> {
    use std::io::Write;

    let now = Local::now().format("%Y-%m-%d at %H:%M:%S").to_string();
    writeln!(f, "# Backup report")?;
    if !s.project_title.is_empty() {
        writeln!(f, "# Project,{}", csv_escape(&s.project_title))?;
    }
    writeln!(f, "# Generated,{}", now)?;

    if !s.company.is_empty() {
        writeln!(f, "# Company,{}", csv_escape(&s.company))?;
    }

    let contact_parts: Vec<&str> = [s.contact_name.as_str(), s.email.as_str(), s.phone.as_str()]
        .iter().filter(|s| !s.is_empty()).copied().collect();
    if !contact_parts.is_empty() {
        writeln!(f, "# Contact,{}", csv_escape(&contact_parts.join(" / ")))?;
    }

    let size_str = format_size(src_total_bytes);
    writeln!(f, "# Source,{} – {}", csv_escape(&src_path.to_string_lossy()), size_str)?;
    for (i, dst) in destinations.iter().enumerate() {
        writeln!(f, "# Destination {},{}",
            i + 1, csv_escape(&dst.to_string_lossy()))?;
    }

    if !location.is_empty() {
        writeln!(f, "# Location,{}", csv_escape(location))?;
    }

    let plain = html_to_plain(comment);
    if !plain.is_empty() {
        for line in plain.lines() {
            if !line.trim().is_empty() {
                writeln!(f, "# Comment,{}", csv_escape(line.trim()))?;
            }
        }
    }

    writeln!(f, "#")?;
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

/// Returns the first run of digits in `s`: `"10"` → `"10"`, `"16 / 24"` → `"16"`.
/// Used for bit depths, where mediainfo may list several values separated by
/// " / " (one per sub-stream) that `clean_number` would glue together.
fn first_number(s: &str) -> String {
    s.chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect()
}

/// Formats a byte count as a human-readable string using decimal SI prefixes
/// (1 KB = 1000 bytes), matching how drive manufacturers and most file managers
/// report capacity.
///
/// - `>= 1 GB` → `"N.NN GB"` (2 decimal places)
/// - `>= 1 MB` → `"N.N MB"` (1 decimal place)
/// - `>= 1 KB` → `"N KB"` (0 decimal places)
/// - `< 1 KB`  → `"N B"`
///
/// `pub` because it is also used by `pdf_report.rs` for the size column.
///
/// ### Digit separator
/// `const KB: u64 = 1_000` — the `_` is a Rust digit separator (like a comma in
/// English or a space in French notation). It improves readability with no effect
/// on the value. `1_000 == 1000`.
pub fn format_size(bytes: u64) -> String {
    const KB: u64 = 1_000;
    const MB: u64 = 1_000 * KB;
    const GB: u64 = 1_000 * MB;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_ul_never_hides_a_known_name() {
        let mut v = "ARRI Wide Gamut 3".to_string();
        set_label(&mut v, "UL 0E06040101010001".into());
        assert_eq!(v, "ARRI Wide Gamut 3");
        set_label(&mut v, "ARRI Wide Gamut 4".into());
        assert_eq!(v, "ARRI Wide Gamut 4");
        let mut e = String::new();
        set_label(&mut e, "UL 0E06040101010001".into());
        assert_eq!(e, "UL 0E06040101010001");
    }

    #[test]
    fn prores_raw_detected_from_fourcc() {
        assert_eq!(video_codec_name("ProRes", "", "", "aprn"), "ProRes RAW");
        assert_eq!(video_codec_name("ProRes", "", "", "aprh"), "ProRes RAW HQ");
        assert_eq!(video_codec_name("ProRes", "RAW HQ", "", ""), "ProRes RAW HQ");
        assert!(is_raw_video("ProRes RAW", "aprn", "mov"));
        assert_eq!(spec_bit_depth("ProRes RAW HQ", ""), Some(12));
    }

    #[test]
    fn prores_profiles_and_bit_depth() {
        assert_eq!(video_codec_name("ProRes", "4444", "", "ap4h"), "ProRes 4444");
        assert_eq!(video_codec_name("ProRes", "422 HQ", "", "apch"), "ProRes 422 HQ");
        assert_eq!(spec_bit_depth("ProRes 4444 XQ", "4444 XQ"), Some(12));
        assert_eq!(spec_bit_depth("ProRes 422 HQ", "422 HQ"), Some(10));
        assert_eq!(spec_bit_depth("H.264", ""), None);
        assert!(!is_raw_video("ProRes 4444", "ap4h", "mov"));
    }

    #[test]
    fn commercial_names() {
        assert_eq!(video_codec_name("VC-3", "RI@LB", "DNxHR LB", "AVdh"), "DNxHR LB");
        assert_eq!(video_codec_name("VC-3", "", "", ""), "DNxHD");
        assert_eq!(video_codec_name("MPEG Video", "4:2:2@High", "XDCAM HD422", ""), "XDCAM HD422");
        assert_eq!(video_codec_name("AVC", "High@L5.1", "", "avc1"), "H.264");
    }

    #[test]
    fn colour_values_and_kelvin() {
        assert_eq!(mediainfo_colour_value("BT.709"), "BT.709");
        assert_eq!(mediainfo_colour_value("0E15000102000000"), "UL 0E15000102000000");
        assert_eq!(format_kelvin("5500"), "5500K");
        assert_eq!(format_kelvin("5500K"), "5500K");
    }

    #[test]
    fn first_number_takes_first_value() {
        assert_eq!(first_number("16 / 24"), "16");
        assert_eq!(first_number("10"), "10");
        assert_eq!(first_number(""), "");
    }
}
