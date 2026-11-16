//! # Module `mhl_report`
//!
//! Writes ASC MHL v2.0 hash lists and keeps their chain file up to date.
//!
//! ## Layout (ASC MHL v2.0)
//! The history of a folder (its "root") lives in `{root}/ascmhl/`:
//! - `NNNN_{rootname}_{YYYY-MM-DD}_{HHMMSS}Z.mhl` — one file per generation,
//!   NNNN strictly increasing within the folder;
//! - `ascmhl_chain.xml` — the list of every generation with the C4 hash of its
//!   file, so a modified or removed generation can be detected.
//!
//! For a card copied "as subfolder", the root is `{dst}/{card}`: the history
//! travels with the card. If the card already carried an `ascmhl/` history,
//! it was copied along in Phase 1 and the new generation continues it: files
//! already recorded with the same hash format are marked `verified` (or
//! `failed` if the hash changed), new ones `original`.
//! Generations are never deleted: the history is append-only.
//!
//! ## Hash list structure (as emitted by `write_generation`)
//! ```xml
//! <?xml version="1.0" encoding="UTF-8"?>
//! <hashlist xmlns="urn:ASC:MHL:v2.0" version="2.0">
//!   <creatorinfo>
//!     <creationdate>2024-06-15T12:00:00Z</creationdate>
//!     <hostname>workstation</hostname>
//!     <tool version="0.1.0">Bartleby</tool>
//!     <author role="organization">Studio Nord</author>
//!     <location>Stage 4</location>
//!     <comment>…</comment>
//!   </creatorinfo>
//!   <processinfo>
//!     <process>transfer</process>
//!     <ignore><pattern>.DS_Store</pattern><pattern>ascmhl</pattern><pattern>ascmhl/</pattern></ignore>
//!   </processinfo>
//!   <hashes>
//!     <hash>
//!       <path size="123456789" lastmodificationdate="2024-06-15T12:00:00Z">rel/path.mov</path>
//!       <xxh128 action="original" hashdate="2024-06-15T12:00:00Z">abcdef…</xxh128>
//!     </hash>
//!   </hashes>
//! </hashlist>
//! ```

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use chrono::{Utc, DateTime};
use crate::settings::Settings;

/// Name of the history folder inside an MHL root.
pub const ASCMHL_DIR: &str = "ascmhl";
const CHAIN_FILE: &str = "ascmhl_chain.xml";

/// True for a relative path (with `/` separators) inside the root's own
/// `ascmhl/` folder. Those files are the history itself: they are never listed
/// in a hash list, nor in a checksum file (the chain file changes on every
/// generation, so its hash would be stale immediately).
pub fn is_history_path(rel: &str) -> bool {
    rel == ASCMHL_DIR || rel.starts_with("ascmhl/")
}

// ── Generations ──────────────────────────────────────────────────────────────

/// Parse the 4-digit generation prefix from an MHL filename (`"0003_NAME_…Z.mhl"` → `3`).
fn parse_generation(filename: &str) -> Option<u32> {
    let stem = filename.strip_suffix(".mhl")?;
    let (gen_str, _) = stem.split_once('_')?;
    if gen_str.len() < 4 || !gen_str.bytes().all(|b| b.is_ascii_digit()) { return None; }
    gen_str.parse::<u32>().ok()
}

/// All generation files of an `ascmhl/` folder, sorted by generation.
fn list_generations(dir: &Path) -> Vec<(u32, PathBuf)> {
    let mut gens: Vec<(u32, PathBuf)> = std::fs::read_dir(dir).into_iter().flatten().flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            parse_generation(&name).map(|g| (g, e.path()))
        })
        .collect();
    gens.sort();
    gens
}

/// The generation with the highest number in an `ascmhl/` folder, if any.
pub fn scan_ascmhl_dir(dir: &Path) -> Option<(PathBuf, u32)> {
    list_generations(dir).pop().map(|(g, p)| (p, g))
}

/// Hashes recorded by the existing generations of `root`, latest wins:
/// relative path → (hash element name, hash value).
pub fn recorded_hashes(root: &Path) -> HashMap<String, (String, String)> {
    let mut map = HashMap::new();
    for (_, path) in list_generations(&root.join(ASCMHL_DIR)) {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let Ok(doc) = roxmltree::Document::parse(&text) else { continue };
        for hash in doc.descendants().filter(|n| n.has_tag_name("hash")) {
            let mut rel = None;
            let mut value = None;
            for c in hash.children().filter(|n| n.is_element()) {
                match c.tag_name().name() {
                    "path" => rel = c.text().map(|t| t.trim().to_string()),
                    // Hash format elements (md5, sha1, xxh64, xxh3, xxh128, c4…)
                    name => if value.is_none() {
                        if let Some(t) = c.text() { value = Some((name.to_string(), t.trim().to_lowercase())); }
                    },
                }
            }
            if let (Some(r), Some(v)) = (rel, value) { map.insert(r, v); }
        }
    }
    map
}

/// Action of a file in a new generation, given the earlier records of the
/// same root: `original` (first time this format is recorded for the path),
/// `verified` (same hash as recorded) or `failed` (hash changed).
pub fn action_for(recorded: &HashMap<String, (String, String)>, rel: &str, elem: &str, hash: &str)
    -> &'static str
{
    match recorded.get(rel) {
        Some((e, h)) if e == elem => if h.eq_ignore_ascii_case(hash) { "verified" } else { "failed" },
        _ => "original",
    }
}

// ── Hostname ──────────────────────────────────────────────────────────────────

/// Machine name for `<hostname>`, without spawning an external command
/// (the `hostname` binary is missing on some minimal Linux installs).
pub fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        let ret = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if ret == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            return String::from_utf8_lossy(&buf[..end]).trim().to_string();
        }
        String::new()
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
}

// ── Hash list writer ─────────────────────────────────────────────────────────

/// One file of a generation: relative path (`/` separators, relative to the
/// root), hash value, and action (`original`, `verified` or `failed`).
pub struct MhlEntry {
    pub rel:    String,
    pub hash:   String,
    pub action: &'static str,
}

/// Writes the next generation of `root`'s history and updates the chain file.
///
/// - `root`      : the folder the hash list describes (contains `ascmhl/`)
/// - `hash_elem` : XML element of the hash format (`"md5"`, `"xxh128"`…)
/// - `process`   : `transfer` (copy) or `in-place` (re-verification)
/// - `comment`   : plain-text comment, `location`: shooting location
///
/// Returns the path of the new `.mhl` file.
pub fn write_generation(
    root:      &Path,
    hash_elem: &str,
    process:   &str,
    entries:   &[MhlEntry],
    comment:   &str,
    location:  &str,
    settings:  &Settings,
    now_utc:   DateTime<Utc>,
) -> io::Result<PathBuf> {
    let dir = root.join(ASCMHL_DIR);
    std::fs::create_dir_all(&dir)?;

    let generation = scan_ascmhl_dir(&dir).map(|(_, g)| g + 1).unwrap_or(1);
    let root_name  = root.file_name().map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty()).unwrap_or_else(|| "root".into());
    let iso        = now_utc.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let filename   = format!("{:04}_{}_{}_{}.mhl", generation, root_name,
                             now_utc.format("%Y-%m-%d"), now_utc.format("%H%M%SZ"));
    let path = dir.join(&filename);
    if path.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists,
            format!("{} already exists", path.display())));
    }

    let mut f = io::BufWriter::new(std::fs::File::create(&path)?);
    writeln!(f, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    writeln!(f, "<hashlist xmlns=\"urn:ASC:MHL:v2.0\" version=\"2.0\">")?;

    writeln!(f, "  <creatorinfo>")?;
    writeln!(f, "    <creationdate>{}</creationdate>", iso)?;
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

    writeln!(f, "  <processinfo>")?;
    writeln!(f, "    <process>{}</process>", xml_escape(process))?;
    writeln!(f, "    <ignore>")?;
    for pat in [".DS_Store", "ascmhl", "ascmhl/"] {
        writeln!(f, "      <pattern>{}</pattern>", pat)?;
    }
    writeln!(f, "    </ignore>")?;
    writeln!(f, "  </processinfo>")?;

    writeln!(f, "  <hashes>")?;
    for e in entries {
        if e.hash.is_empty() || is_history_path(&e.rel) { continue; }
        let file = root.join(e.rel.replace('/', std::path::MAIN_SEPARATOR_STR));
        let meta = std::fs::metadata(&file).ok();
        let mtime_iso = meta.as_ref().and_then(|m| m.modified().ok())
            .map(|t| { let dt: DateTime<Utc> = t.into(); dt.format("%Y-%m-%dT%H:%M:%SZ").to_string() });
        writeln!(f, "    <hash>")?;
        write!(f, "      <path")?;
        if let Some(m) = &meta { write!(f, " size=\"{}\"", m.len())?; }
        if let Some(t) = &mtime_iso { write!(f, " lastmodificationdate=\"{}\"", t)?; }
        writeln!(f, ">{}</path>", xml_escape(&e.rel))?;
        writeln!(f, "      <{elem} action=\"{}\" hashdate=\"{}\">{}</{elem}>",
                 e.action, iso, e.hash, elem = hash_elem)?;
        writeln!(f, "    </hash>")?;
    }
    writeln!(f, "  </hashes>")?;
    writeln!(f, "</hashlist>")?;
    f.into_inner().map_err(|e| e.into_error())?.sync_all()?;

    update_chain(&dir)?;
    Ok(path)
}

// ── Chain file ────────────────────────────────────────────────────────────────

/// Brings `ascmhl_chain.xml` up to date: existing entries are kept as they are
/// (their C4 is the reference a tampered generation is checked against), and
/// every generation file not yet listed is added with its C4 hash.
/// A chain file that cannot be parsed is left untouched and reported.
fn update_chain(dir: &Path) -> io::Result<()> {
    let chain_path = dir.join(CHAIN_FILE);
    let mut chain: Vec<(u32, String, String)> = Vec::new();
    if chain_path.exists() {
        let text = std::fs::read_to_string(&chain_path)?;
        let doc = roxmltree::Document::parse(&text).map_err(|e| io::Error::new(
            io::ErrorKind::InvalidData, format!("{}: {} — chain not updated", chain_path.display(), e)))?;
        for hl in doc.descendants().filter(|n| n.has_tag_name("hashlist")) {
            let seq = hl.attribute("sequencenr").and_then(|s| s.parse().ok()).unwrap_or(0);
            let child = |tag: &str| hl.children().find(|n| n.has_tag_name(tag))
                .and_then(|n| n.text()).map(|t| t.trim().to_string()).unwrap_or_default();
            chain.push((seq, child("path"), child("c4")));
        }
    }
    let mut changed = !chain_path.exists();
    for (gen, path) in list_generations(dir) {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
        if chain.iter().any(|(_, p, _)| *p == name) { continue; }
        let c4 = crate::copy_engine::hash_file_path(&path, crate::copy_engine::HashAlgo::C4)?;
        chain.push((gen, name, c4));
        changed = true;
    }
    if !changed { return Ok(()); }
    chain.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    // Write to a temporary file, then rename: a crash never leaves a
    // truncated chain file behind.
    let tmp = dir.join(format!(".{}.tmp", CHAIN_FILE));
    {
        let mut f = io::BufWriter::new(std::fs::File::create(&tmp)?);
        writeln!(f, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
        writeln!(f, "<ascmhldirectory xmlns=\"urn:ASC:MHL:DIRECTORY:v2.0\">")?;
        for (seq, name, c4) in &chain {
            writeln!(f, "  <hashlist sequencenr=\"{}\">", seq)?;
            writeln!(f, "    <path>{}</path>", xml_escape(name))?;
            writeln!(f, "    <c4>{}</c4>", xml_escape(c4))?;
            writeln!(f, "  </hashlist>")?;
        }
        writeln!(f, "</ascmhldirectory>")?;
        f.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    }
    std::fs::rename(&tmp, &chain_path)
}

// ── XML helpers ───────────────────────────────────────────────────────────────

fn xml_escape(s: &str) -> String {
    s.replace('&',  "&amp;")
     .replace('<',  "&lt;")
     .replace('>',  "&gt;")
     .replace('"',  "&quot;")
     .replace('\'', "&apos;")
}


#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bartleby_mhl_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn generations_are_sequential_and_chained() {
        let root = tmp("chain").join("A001");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("clip.mov"), b"data").unwrap();
        std::fs::write(root.join("empty.txt"), b"").unwrap();
        let s = Settings::default();
        let e = |a| vec![MhlEntry { rel: "clip.mov".into(), hash: "abcd".into(), action: a },
                         MhlEntry { rel: "empty.txt".into(), hash: "ef01".into(), action: a }];
        let t1 = chrono::Utc::now();
        let p1 = write_generation(&root, "xxh64", "transfer", &e("original"), "", "", &s, t1).unwrap();
        let t2 = t1 + chrono::Duration::seconds(1);
        let p2 = write_generation(&root, "xxh64", "in-place", &e("verified"), "", "", &s, t2).unwrap();
        assert!(p1.file_name().unwrap().to_string_lossy().starts_with("0001_A001_"));
        assert!(p2.file_name().unwrap().to_string_lossy().starts_with("0002_A001_"));

        let mhl = std::fs::read_to_string(&p1).unwrap();
        assert!(mhl.contains("size=\"0\""), "0-byte file must carry size=0:\n{mhl}");

        let chain = std::fs::read_to_string(root.join("ascmhl").join(CHAIN_FILE)).unwrap();
        assert!(chain.contains("sequencenr=\"1\"") && chain.contains("sequencenr=\"2\""), "{chain}");
        let c4 = crate::copy_engine::hash_file_path(&p1, crate::copy_engine::HashAlgo::C4).unwrap();
        assert!(chain.contains(&c4), "{chain}");

        let rec = recorded_hashes(&root);
        assert_eq!(action_for(&rec, "clip.mov", "xxh64", "ABCD"), "verified");
        assert_eq!(action_for(&rec, "clip.mov", "xxh64", "ffff"), "failed");
        assert_eq!(action_for(&rec, "clip.mov", "md5", "abcd"), "original");
        assert_eq!(action_for(&rec, "new.mov", "xxh64", "abcd"), "original");
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn history_paths_and_generation_names() {
        assert!(is_history_path("ascmhl/ascmhl_chain.xml"));
        assert!(!is_history_path("ascmhl_notes.txt"));
        assert!(!is_history_path("sub/ascmhl/x.mhl"));
        assert_eq!(parse_generation("0012_A_B_2024-01-01_000000Z.mhl"), Some(12));
        assert_eq!(parse_generation("notes_1.mhl"), None);
    }
}
