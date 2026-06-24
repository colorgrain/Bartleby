//! # Module `mhl_report`
//!
//! Generates ASC MHL v2.0 (Media Hash List) XML files for professional
//! media workflows. One MHL file is written per destination.
//!
//! ## File location
//! `{dst}/ascmhl/{NNNN}_{report_name}_{date}_{time}Z.mhl`
//! where NNNN is the generation number (auto-incremented) and report_name
//! is the destination folder name (not the source device name).
//!
//! ## Generational chain
//! If the source directory already has an MHL, the new MHL references it
//! via a `<references>` block — building a cryptographic chain of custody.
//! If the destination already has an MHL for this source, the user is
//! prompted (via `Msg::MhlConflict`) to Replace, Keep-both, or Skip.
//!
//! ## ASC MHL v2.0 structure (as emitted by `write_mhl`)
//! ```xml
//! <?xml version="1.0" encoding="UTF-8"?>
//! <hashlist xmlns="urn:ASC:MHL:v2.0" version="2.0">
//!   <creatorinfo>
//!     <creationdate>2024-06-15T12:00:00Z</creationdate>
//!     <hostname>workstation</hostname>
//!     <tool version="0.1.0">Bartleby</tool>
//!     <author role="organization">Studio Nord</author>
//!     <author email="a@b.c" phone="…">Alex</author>
//!     <location>Stage 4</location>
//!     <comment>…</comment>
//!   </creatorinfo>
//!   <processinfo><process>transfer</process></processinfo>
//!   <hashes>
//!     <hash>
//!       <path size="123456789" lastmodificationdate="2024-06-15T12:00:00Z">rel/path.mov</path>
//!       <xxh128 action="original">abcdef…</xxh128>
//!     </hash>
//!   </hashes>
//!   <!-- Generational chain: only when the parent MHL is within this dst's scope.
//!        The path is relative to the destination root; the hash is always C4. -->
//!   <references>
//!     <hashlistreference>
//!       <path>ascmhl/0001_…Z.mhl</path>
//!       <c4>c4hash_of_that_mhl_file</c4>
//!     </hashlistreference>
//!   </references>
//! </hashlist>
//! ```

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use chrono::{Utc, DateTime};
use crate::settings::Settings;

// ── Generational chain helpers ────────────────────────────────────────────────

/// Reference to a parent MHL file — used to build the generational chain.
pub struct MhlRef {
    /// Absolute path to the source MHL file.
    /// Used to compute a relative path (if within dst scope) and the C4 hash.
    pub path:       String,
    /// Generation number parsed from the source MHL filename prefix.
    pub generation: u32,
}

/// Parse the 4-digit generation prefix from an MHL filename (`"0003_NAME_…Z.mhl"` → `3`).
fn parse_generation(filename: &str) -> Option<u32> {
    let stem = filename.strip_suffix(".mhl")?;
    let (gen_str, _) = stem.split_once('_')?;
    gen_str.parse::<u32>().ok()
}

/// Scan a directory for `.mhl` files and return the one with the highest
/// generation number as `(absolute_path, generation)`.
/// Returns `None` when the directory is absent, empty, or contains no `.mhl` files.
pub fn scan_ascmhl_dir(dir: &Path) -> Option<(PathBuf, u32)> {
    let mut best: Option<(PathBuf, u32)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".mhl") { continue; }
        if let Some(gen) = parse_generation(&name) {
            if best.as_ref().map_or(true, |(_, g)| gen > *g) {
                best = Some((entry.path(), gen));
            }
        }
    }
    best
}

/// Find the highest-generation MHL in `{dst}/ascmhl/` whose filename contains
/// `_{src_name}_` — i.e., a previous copy of the same source to this destination.
/// Returns `(path, generation)` or `None`.
pub fn find_dst_mhl_for_src(dst: &Path, report_name: &str) -> Option<(PathBuf, u32)> {
    let needle = format!("_{}_", report_name);
    let mut best: Option<(PathBuf, u32)> = None;
    for entry in std::fs::read_dir(dst.join("ascmhl")).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".mhl") || !name.contains(&needle) { continue; }
        if let Some(gen) = parse_generation(&name) {
            if best.as_ref().map_or(true, |(_, g)| gen > *g) {
                best = Some((entry.path(), gen));
            }
        }
    }
    best
}

// ── Hostname ──────────────────────────────────────────────────────────────────

pub fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

// ── MHL writer ────────────────────────────────────────────────────────────────

/// Writes an ASC MHL v2.0 file into `{dst}/ascmhl/` and returns its path.
///
/// ## Parameters
/// - `dst`         : destination root directory
/// - `report_name` : destination folder name (used in filename and metadata)
/// - `entries`     : `(relative_path, hash_hex)` for each file
/// - `hash_elem`   : XML element name for the hash (`"md5"`, `"xxh128"`, etc.)
/// - `comment`     : per-job comment (HTML) — stripped to plain text for XML
/// - `location`    : shooting location string
/// - `settings`    : user preferences (name, company, email, phone)
/// - `generation`  : file sequence number (written as the `NNNN` filename prefix)
/// - `now_utc`     : timestamp shared across all report files for this copy operation
/// - `src_ref`     : optional parent MHL reference for the generational chain
pub fn write_mhl(
    dst:         &Path,
    report_name: &str,
    _src_path:   &Path,
    entries:     &[(String, String)],
    hash_elem:   &str,
    comment:     &str,
    location:    &str,
    settings:    &Settings,
    generation:  u32,
    now_utc:     DateTime<Utc>,
    src_ref:     Option<&MhlRef>,
) -> io::Result<PathBuf> {
    let date_str   = now_utc.format("%Y-%m-%d").to_string();
    let time_str   = now_utc.format("%H%M%SZ").to_string();
    let finish_iso = now_utc.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let dir = dst.join("ascmhl");
    std::fs::create_dir_all(&dir)?;

    let filename = format!("{:04}_{}_{}_{}.mhl", generation, report_name, date_str, time_str);
    let path     = dir.join(&filename);
    let mut f    = std::fs::File::create(&path)?;

    writeln!(f, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    writeln!(f, "<hashlist xmlns=\"urn:ASC:MHL:v2.0\" version=\"2.0\">")?;

    // ── creatorinfo ───────────────────────────────────────────────────────────
    writeln!(f, "  <creatorinfo>")?;
    writeln!(f, "    <creationdate>{}</creationdate>", finish_iso)?;
    writeln!(f, "    <hostname>{}</hostname>", xml_escape(&hostname()))?;
    writeln!(f, "    <tool version=\"{}\">Bartleby</tool>", xml_escape(crate::VERSION))?;
    let company = settings.company.trim();
    let name    = settings.contact_name.trim();
    let email   = settings.email.trim();
    let phone   = settings.phone.trim();
    if !company.is_empty() {
        writeln!(f, "    <author role=\"organization\">{}</author>", xml_escape(company))?;
    }
    if !name.is_empty() || !email.is_empty() || !phone.is_empty() {
        write!(f, "    <author")?;
        if !email.is_empty() { write!(f, " email=\"{}\"", xml_escape(email))?; }
        if !phone.is_empty() { write!(f, " phone=\"{}\"", xml_escape(phone))?; }
        writeln!(f, ">{}</author>", xml_escape(name))?;
    }
    if !location.is_empty() {
        writeln!(f, "    <location>{}</location>", xml_escape(location))?;
    }
    if !comment.is_empty() {
        writeln!(f, "    <comment>{}</comment>", xml_escape(comment))?;
    }
    writeln!(f, "  </creatorinfo>")?;

    // ── processinfo ───────────────────────────────────────────────────────────
    writeln!(f, "  <processinfo>")?;
    writeln!(f, "    <process>transfer</process>")?;
    writeln!(f, "  </processinfo>")?;

    // ── hashes ────────────────────────────────────────────────────────────────
    writeln!(f, "  <hashes>")?;
    for (rel, hash) in entries {
        if hash.is_empty() { continue; }

        let rel_native = rel.replace('/', std::path::MAIN_SEPARATOR_STR);
        let dst_file   = dst.join(&rel_native);

        let (size, mtime_iso) = if let Ok(meta) = std::fs::metadata(&dst_file) {
            let size = meta.len();
            let mtime = meta.modified()
                .ok()
                .map(|t| { let dt: DateTime<Utc> = t.into(); dt.format("%Y-%m-%dT%H:%M:%SZ").to_string() })
                .unwrap_or_default();
            (size, mtime)
        } else {
            (0, String::new())
        };

        writeln!(f, "    <hash>")?;
        write!(f, "      <path")?;
        if size > 0              { write!(f, " size=\"{}\"", size)?; }
        if !mtime_iso.is_empty() { write!(f, " lastmodificationdate=\"{}\"", mtime_iso)?; }
        writeln!(f, ">{}</path>", xml_escape(rel))?;
        writeln!(f, "      <{} action=\"original\">{}</{}>", hash_elem, hash, hash_elem)?;
        writeln!(f, "    </hash>")?;
    }
    writeln!(f, "  </hashes>")?;

    // ── references (generational chain) ───────────────────────────────────────
    // The reference path must be relative to the scope of this manifest (dst root).
    // Cross-volume references (source on a different drive) are omitted since
    // RelativePathType cannot express them. The C4 hash is required by the spec.
    if let Some(r) = src_ref {
        let abs_ref = Path::new(&r.path);
        if let Ok(rel) = abs_ref.strip_prefix(dst) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let c4 = crate::copy_engine::hash_file_path(abs_ref, crate::copy_engine::HashAlgo::C4)
                .unwrap_or_default();
            if !c4.is_empty() {
                writeln!(f, "  <references>")?;
                writeln!(f, "    <hashlistreference>")?;
                writeln!(f, "      <path>{}</path>", xml_escape(&rel_str))?;
                writeln!(f, "      <c4>{}</c4>", c4)?;
                writeln!(f, "    </hashlistreference>")?;
                writeln!(f, "  </references>")?;
            }
        }
    }

    writeln!(f, "</hashlist>")?;
    Ok(path)
}

// ── XML helpers ───────────────────────────────────────────────────────────────

fn xml_escape(s: &str) -> String {
    s.replace('&',  "&amp;")
     .replace('<',  "&lt;")
     .replace('>',  "&gt;")
     .replace('"',  "&quot;")
     .replace('\'', "&apos;")
}

