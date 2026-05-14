//! # Module `mhl_report`
//!
//! Generates ASC MHL v2.0 (Media Hash List) XML files for professional
//! media workflows. One MHL file is written per destination.
//!
//! ## File location
//! `{dst}/ascmhl/{NNNN}_{src_name}_{date}_{time}Z.mhl`
//! where NNNN is the generation number (auto-incremented).
//!
//! ## Generational chain
//! If the source directory already has an MHL, the new MHL references it
//! via a `<references>` block — building a cryptographic chain of custody.
//! If the destination already has an MHL for this source, the user is
//! prompted (via `Msg::MhlConflict`) to Replace, Keep-both, or Skip.
//!
//! ## ASC MHL v2.0 structure
//! ```xml
//! <?xml version="1.0" encoding="UTF-8"?>
//! <hashlist xmlns="urn:ASC:MHL:v2.0" version="2.0">
//!   <creatorinfo>…</creatorinfo>
//!   <processinfo><process>copy</process></processinfo>
//!   <hashes>
//!     <hash>
//!       <file><path>rel/path.mov</path><size>123456789</size></file>
//!       <xxh128>abcdef…</xxh128>
//!       <lastmodificationdate>2024-06-15T12:00:00Z</lastmodificationdate>
//!     </hash>
//!   </hashes>
//!   <references>
//!     <reference>
//!       <path>/abs/path/to/source/ascmhl/0001_…Z.mhl</path>
//!       <xxh128>hash_of_that_mhl_file</xxh128>
//!     </reference>
//!   </references>
//! </hashlist>
//! ```

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use chrono::{Local, Utc, DateTime};
use crate::settings::Settings;

// ── Generational chain helpers ────────────────────────────────────────────────

/// Reference to a parent MHL file — used to build the generational chain.
pub struct MhlRef {
    /// Absolute path to the source MHL file (used verbatim in `<references>`).
    pub path:       String,
    /// Hash of the source MHL file itself, hex-encoded, using the same algorithm
    /// as this copy job. Empty string when the algorithm is unsupported for references.
    pub hash:       String,
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
pub fn find_dst_mhl_for_src(dst: &Path, src_name: &str) -> Option<(PathBuf, u32)> {
    let needle = format!("_{}_", src_name);
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

// ── MHL writer ────────────────────────────────────────────────────────────────

/// Writes an ASC MHL v2.0 file into `{dst}/ascmhl/` and returns its path.
///
/// ## Parameters
/// - `dst`         : destination root directory
/// - `src_name`    : source folder name (used in filename)
/// - `entries`     : `(relative_path, hash_hex)` for each file
/// - `hash_elem`   : XML element name for the hash (`"md5"`, `"xxh128"`, etc.)
/// - `comment`     : per-job comment (HTML) — stripped to plain text for XML
/// - `location`    : shooting location string
/// - `settings`    : user preferences (name, company, email, phone)
/// - `generation`  : file sequence number (written as the `NNNN` filename prefix)
/// - `src_ref`     : optional parent MHL reference for the generational chain
pub fn write_mhl(
    dst:        &Path,
    src_name:   &str,
    entries:    &[(String, String)],
    hash_elem:  &str,
    comment:    &str,
    location:   &str,
    settings:   &Settings,
    generation: u32,
    src_ref:    Option<&MhlRef>,
) -> io::Result<PathBuf> {
    let now_local: DateTime<Local> = Local::now();
    let now_utc:   DateTime<Utc>   = Utc::now();
    let date_str   = now_local.format("%Y-%m-%d").to_string();
    let time_str   = now_utc.format("%H%M%SZ").to_string();
    let finish_iso = now_utc.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let dir = dst.join("ascmhl");
    std::fs::create_dir_all(&dir)?;

    let filename = format!("{:04}_{}_{}_{}.mhl", generation, src_name, date_str, time_str);
    let path     = dir.join(&filename);
    let mut f    = std::fs::File::create(&path)?;

    writeln!(f, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    writeln!(f, "<hashlist xmlns=\"urn:ASC:MHL:v2.0\" version=\"2.0\">")?;

    // ── creatorinfo ───────────────────────────────────────────────────────────
    writeln!(f, "  <creatorinfo>")?;
    writeln!(f, "    <name>Bartleby {}</name>", crate::VERSION)?;
    writeln!(f, "    <finishdate>{}</finishdate>", finish_iso)?;
    writeln!(f, "    <author>")?;
    let author_name = if !settings.company.is_empty() && !settings.contact_name.is_empty() {
        format!("{} / {}", xml_escape(&settings.company), xml_escape(&settings.contact_name))
    } else if !settings.contact_name.is_empty() {
        xml_escape(&settings.contact_name)
    } else if !settings.company.is_empty() {
        xml_escape(&settings.company)
    } else {
        String::new()
    };
    if !author_name.is_empty() {
        writeln!(f, "      <name>{}</name>", author_name)?;
    }
    if !settings.email.is_empty() {
        writeln!(f, "      <email>{}</email>", xml_escape(&settings.email))?;
    }
    if !settings.phone.is_empty() {
        writeln!(f, "      <phone>{}</phone>", xml_escape(&settings.phone))?;
    }
    writeln!(f, "    </author>")?;
    if !location.is_empty() {
        writeln!(f, "    <location>{}</location>", xml_escape(location))?;
    }
    let plain_comment = html_to_plain(comment);
    if !plain_comment.is_empty() {
        writeln!(f, "    <comment>{}</comment>", xml_escape(&plain_comment))?;
    }
    writeln!(f, "  </creatorinfo>")?;

    // ── processinfo ───────────────────────────────────────────────────────────
    writeln!(f, "  <processinfo>")?;
    writeln!(f, "    <process>copy</process>")?;
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
        writeln!(f, "      <file>")?;
        writeln!(f, "        <path>{}</path>", xml_escape(rel))?;
        writeln!(f, "        <size>{}</size>", size)?;
        writeln!(f, "      </file>")?;
        writeln!(f, "      <{}>{}</{}>", hash_elem, hash, hash_elem)?;
        if !mtime_iso.is_empty() {
            writeln!(f, "      <lastmodificationdate>{}</lastmodificationdate>", mtime_iso)?;
        }
        writeln!(f, "    </hash>")?;
    }
    writeln!(f, "  </hashes>")?;

    // ── references (generational chain) ───────────────────────────────────────
    if let Some(r) = src_ref {
        writeln!(f, "  <references>")?;
        writeln!(f, "    <reference>")?;
        writeln!(f, "      <path>{}</path>", xml_escape(&r.path))?;
        if !r.hash.is_empty() {
            writeln!(f, "      <{}>{}</{}>", hash_elem, r.hash, hash_elem)?;
        }
        writeln!(f, "    </reference>")?;
        writeln!(f, "  </references>")?;
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
                    result.push(' ');
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
        .trim()
        .to_string()
}
