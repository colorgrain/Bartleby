//! # Module `verify_engine`
//!
//! Verification tool: parses checksum files (`.md5`, `.sha1`, `.xxh128`, …)
//! and ASC MHL v2.0 files (`.mhl`), re-hashes every listed file, and reports
//! pass/fail per entry.
//!
//! After verifying an MHL the caller can write a post-verification MHL
//! via `write_post_verify_mhl()` — same `ascmhl/` directory, next generation,
//! `<process>verify</process>`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use serde::{Serialize, Deserialize};
use tauri::Emitter;
use crate::copy_engine::{HashAlgo, PauseCancel};

// ── Public data structures ────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VerifyEntry {
    pub rel_path:  String,
    pub expected:  String,          // hash from the checksum/MHL file
    pub computed:  String,          // hash we computed (empty when file missing)
    pub file_size: u64,             // actual file size (0 when missing)
    pub size_ok:   Option<bool>,    // MHL only: expected_size == file_size
    pub mtime_ok:  Option<bool>,    // MHL only: expected mtime matches fs mtime
    pub status:    String,          // "ok" | "mismatch" | "missing" | "error"
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct MhlMeta {
    pub creator:        String,
    pub finish_date:    String,
    pub author_company: String,   // populated from <author role="organization">
    pub author_name:    String,
    pub author_email:   String,
    pub author_phone:   String,
    pub location:       String,
    pub comment:        String,
    pub process:        String,   // "in-place" | "transfer" | "flatten"
    pub generation:     u32,
    pub parent_ref:     Option<String>,
    pub hash_algo:      String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct VerifyResult {
    pub file_path:     String,      // absolute path to the verified checksum/MHL file
    pub file_type:     String,      // "checksum" | "mhl"
    pub algo:          String,      // "md5" | "sha1" | "xxh128" …
    pub mhl_meta:      Option<MhlMeta>,
    pub entries:       Vec<VerifyEntry>,
    pub total:         usize,
    pub ok_count:      usize,
    pub fail_count:    usize,
    pub missing_count: usize,
}

/// Pre-parse result: file list from a checksum/MHL file without hashing.
/// Used to populate the table immediately when a file is loaded.
#[derive(Serialize, Clone, Debug)]
pub struct ParsedFileEntry {
    pub rel_path:       String,
    pub expected_hash:  String,
    pub expected_size:  u64,
    pub expected_mtime: Option<String>,
    pub algo:           String,
}

#[derive(Serialize, Clone, Debug)]
pub struct FileListResult {
    pub file_type: String,
    pub algo:      String,
    pub mhl_meta:  Option<MhlMeta>,
    pub mhl_chain: Vec<MhlMeta>,  // all generations in the ascmhl/ dir, sorted ascending
    pub entries:   Vec<ParsedFileEntry>,
}

#[derive(Serialize, Clone)]
struct VerifyProgress {
    fraction: f64,
    label:    String,
}

// Per-file result emitted during verification so the table updates row by row
#[derive(Serialize, Clone)]
struct VerifyEntryEvent {
    index:     usize,
    computed:  String,
    status:    String,
    file_size: u64,
    size_ok:   Option<bool>,
    mtime_ok:  Option<bool>,
}

// ── Pre-parse (no hashing) ────────────────────────────────────────────────────

/// Parse a checksum or MHL file and return the file list without hashing.
/// Called when a file is dropped/browsed so the table populates immediately.
pub fn parse_file(file_path: PathBuf) -> io::Result<FileListResult> {
    let ext = file_path.extension()
        .and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext == "mhl" {
        parse_mhl_file_list(&file_path)
    } else {
        parse_checksum_file_list(&file_path)
    }
}

fn parse_checksum_file_list(file_path: &Path) -> io::Result<FileListResult> {
    let ext  = file_path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let algo = algo_from_ext(&ext);
    if algo == HashAlgo::None {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            format!("Unrecognised checksum extension: .{}", ext)));
    }
    let raw = parse_checksum_file(file_path)?;
    let entries = raw.iter().map(|e| ParsedFileEntry {
        rel_path:       e.rel_path.clone(),
        expected_hash:  e.expected.clone(),
        expected_size:  0,
        expected_mtime: None,
        algo:           ext.clone(),
    }).collect();
    Ok(FileListResult { file_type: "checksum".into(), algo: ext, mhl_meta: None, mhl_chain: vec![], entries })
}

fn parse_mhl_file_list(file_path: &Path) -> io::Result<FileListResult> {
    let doc   = parse_mhl_file(file_path)?;
    let chain = build_mhl_chain(file_path);
    let algo_str = doc.algo.mhl_element().unwrap_or("").to_string();
    let entries = doc.entries.iter().map(|e| ParsedFileEntry {
        rel_path:       e.rel_path.clone(),
        expected_hash:  e.expected_hash.clone(),
        expected_size:  e.expected_size,
        expected_mtime: e.expected_mtime.clone(),
        algo:           e.algo.mhl_element().unwrap_or("").to_string(),
    }).collect();
    Ok(FileListResult {
        file_type: "mhl".into(),
        algo:      algo_str,
        mhl_meta:  Some(doc.meta),
        mhl_chain: chain,
        entries,
    })
}

/// Parse only creatorinfo/processinfo from an MHL — no hash entries needed for the chain display.
fn parse_mhl_meta_only(path: &Path) -> io::Result<MhlMeta> {
    let text = std::fs::read_to_string(path)?;
    let doc  = roxmltree::Document::parse(&text)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let root = doc.root_element();
    let mut meta = MhlMeta::default();
    for child in root.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "creatorinfo" => {
                for ci in child.children().filter(|n| n.is_element()) {
                    match ci.tag_name().name() {
                        "creationdate" => meta.finish_date = text_of(&ci),
                        "tool"         => meta.creator     = text_of(&ci),
                        "location"     => meta.location    = text_of(&ci),
                        "comment"      => meta.comment     = text_of(&ci),
                        "author" => {
                            if ci.attribute("role") == Some("organization") {
                                meta.author_company = text_of(&ci);
                            } else {
                                meta.author_name = text_of(&ci);
                                if let Some(e) = ci.attribute("email") { meta.author_email = e.to_string(); }
                                if let Some(p) = ci.attribute("phone") { meta.author_phone = p.to_string(); }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "processinfo" => {
                for pi in child.children().filter(|n| n.is_element()) {
                    if pi.tag_name().name() == "process" {
                        meta.process = text_of(&pi);
                    }
                }
            }
            "hashes" => {
                'algo_scan: for hash_el in child.children().filter(|n| n.is_element()) {
                    if hash_el.tag_name().name() != "hash" { continue; }
                    for he in hash_el.children().filter(|n| n.is_element()) {
                        if HashAlgo::from_mhl_element(he.tag_name().name()) != HashAlgo::None {
                            meta.hash_algo = he.tag_name().name().to_string();
                            break 'algo_scan;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        meta.generation = name.split('_').next()
            .and_then(|g| g.parse::<u32>().ok()).unwrap_or(0);
    }
    Ok(meta)
}

/// Collect all MHL generations from the same ascmhl/ directory, sorted ascending.
fn build_mhl_chain(mhl_path: &Path) -> Vec<MhlMeta> {
    let dir = match mhl_path.parent() { Some(d) => d, None => return vec![] };
    let mut chain = Vec::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("mhl") {
                if let Ok(meta) = parse_mhl_meta_only(&p) {
                    chain.push(meta);
                }
            }
        }
    }
    chain.sort_by_key(|m| m.generation);
    chain
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Spawn a background thread that verifies `file_path` and emits Tauri events
/// to `win`:
/// - `"verify-progress"` → `VerifyProgress` (fraction 0.0–1.0, label)
/// - `"verify-done"`     → `VerifyResult`
/// - `"verify-error"`    → String error message
/// `running` is cleared when the thread ends (see AppState::verify_running).
pub fn run(file_path: PathBuf, win: tauri::WebviewWindow, pc: Arc<PauseCancel>,
           running: Arc<std::sync::atomic::AtomicBool>) {
    std::thread::spawn(move || {
        // Cleared when this closure ends, even on panic.
        struct Clear(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Clear {
            fn drop(&mut self) { self.0.store(false, std::sync::atomic::Ordering::SeqCst); }
        }
        let _clear = Clear(running);
        let ext = file_path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let result = if ext == "mhl" {
            run_mhl_verify(&file_path, &win, &pc)
        } else {
            run_checksum_verify(&file_path, &win, &pc)
        };

        match result {
            Ok(r)  => { let _ = win.emit("verify-done",  r); }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {
                let _ = win.emit("verify-cancelled", ());
            }
            Err(e) => { let _ = win.emit("verify-error", e.to_string()); }
        }
    });
}

// ── Checksum file verification ────────────────────────────────────────────────

struct RawEntry {
    rel_path: String,
    expected: String,
}

fn parse_checksum_file(path: &Path) -> io::Result<Vec<RawEntry>> {
    let text = std::fs::read_to_string(path)?;
    // A UTF-8 BOM (files saved by some Windows tools) would otherwise stick to
    // the first hash and make the first file a false mismatch.
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(&text);
    Ok(text.lines().filter_map(parse_checksum_line).collect())
}

/// Parses one line of a checksum file. Accepted formats:
/// - GNU / coreutils: `<hash>  <path>` (text) or `<hash> *<path>` (binary);
///   the path is taken verbatim after the separator, so names with double
///   spaces or leading / trailing spaces are kept; a line starting with `\`
///   has `\\` and `\n` escapes in the path (md5sum convention);
/// - BSD / `--tag`: `MD5 (<path>) = <hash>`;
/// - xxhsum's XXH3 prefix: `XXH3_<hash>  <path>`.
/// Comments (`#`) and blank lines are skipped.
fn parse_checksum_line(line: &str) -> Option<RawEntry> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.trim().is_empty() || line.starts_with('#') { return None; }

    // BSD style: ALGO (path) = hash
    if let (Some(open), Some(close)) = (line.find(" ("), line.rfind(") = ")) {
        let algo = &line[..open];
        if open < close && !algo.is_empty() && algo.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            let rel  = &line[open + 2..close];
            let hash = line[close + 4..].trim();
            if !rel.is_empty() && !hash.is_empty() {
                return Some(RawEntry { rel_path: rel.to_string(), expected: hash.to_string() });
            }
        }
    }

    // GNU style
    let (escaped, line) = match line.strip_prefix('\\') {
        Some(rest) => (true, rest),
        None       => (false, line),
    };
    let sp = line.find(' ')?;
    let hash = &line[..sp];
    let rest = &line[sp + 1..];
    let rel = match rest.chars().next() {
        Some(' ') | Some('*') => &rest[1..],
        _ => rest, // tolerate a single space
    };
    if hash.is_empty() || rel.is_empty() { return None; }
    let hash = hash.strip_prefix("XXH3_").unwrap_or(hash);
    let rel = if escaped {
        let mut out = String::with_capacity(rel.len());
        let mut it = rel.chars();
        while let Some(c) = it.next() {
            if c == '\\' {
                match it.next() {
                    Some('n')  => out.push('\n'),
                    Some('\\') => out.push('\\'),
                    Some(o)    => { out.push('\\'); out.push(o); }
                    None       => out.push('\\'),
                }
            } else { out.push(c); }
        }
        out
    } else { rel.to_string() };
    Some(RawEntry { rel_path: rel, expected: hash.to_string() })
}

/// Status of an entry whose path leaves the checked folder.
const REFUSED_PATH: &str = "error: path outside the checked folder";

/// Resolves an entry path (with `/` separators) inside `root`. Absolute paths
/// and `..` components are refused: a checksum file or MHL must not make
/// Bartleby read files elsewhere on the disk.
fn resolve_entry(root: &Path, rel: &str) -> Option<PathBuf> {
    use std::path::Component;
    let rel_native = PathBuf::from(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
    if rel_native.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
        Some(root.join(rel_native))
    } else {
        None
    }
}

fn algo_from_ext(ext: &str) -> HashAlgo {
    match ext {
        "md5"   => HashAlgo::Md5,
        "sha1"  => HashAlgo::Sha1,
        "xxh64" => HashAlgo::Xxh64,
        "xxh3"  => HashAlgo::Xxh3_64,
        "xxh128"=> HashAlgo::Xxh128,
        "c4"    => HashAlgo::C4,
        _       => HashAlgo::None,
    }
}

fn run_checksum_verify(file_path: &Path, win: &tauri::WebviewWindow, pc: &Arc<PauseCancel>) -> io::Result<VerifyResult> {
    let ext  = file_path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let algo = algo_from_ext(&ext);
    if algo == HashAlgo::None {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            format!("Unrecognised checksum extension: .{}", ext)));
    }

    let raw = parse_checksum_file(file_path)?;
    if raw.is_empty() {
        // Never report "0 failures" for a file we could not read any entry from.
        return Err(io::Error::new(io::ErrorKind::InvalidData,
            "No usable entry in this checksum file (empty, or a format Bartleby does not read)."));
    }
    // Files are resolved relative to the checksum file's parent directory
    let root = file_path.parent().unwrap_or(Path::new("."));
    let total = raw.len();
    let mut entries = Vec::with_capacity(total);

    for (i, raw_e) in raw.iter().enumerate() {
        pc.wait_if_paused().map_err(|_| io::Error::new(io::ErrorKind::Interrupted, "cancelled"))?;

        let _ = win.emit("verify-progress", VerifyProgress {
            fraction: i as f64 / total.max(1) as f64,
            label:    format!("Verifying {} / {}", i + 1, total),
        });

        let abs = resolve_entry(root, &raw_e.rel_path);

        let (computed, file_size, status) = match abs.as_deref() {
          None => (String::new(), 0, REFUSED_PATH.to_string()),
          Some(abs) => match crate::copy_engine::hash_file_verified(abs, algo, pc) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return Err(e),
            Ok(hash) => {
                let size = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
                let ok   = hash.eq_ignore_ascii_case(&raw_e.expected);
                (hash, size, if ok { "ok" } else { "mismatch" }.to_string())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                (String::new(), 0, "missing".to_string())
            }
            Err(e) => (String::new(), 0, format!("error: {}", e)),
          },
        };

        let _ = win.emit("verify-entry", VerifyEntryEvent {
            index: i, computed: computed.clone(), status: status.clone(),
            file_size, size_ok: None, mtime_ok: None,
        });
        entries.push(VerifyEntry {
            rel_path:  raw_e.rel_path.clone(),
            expected:  raw_e.expected.clone(),
            computed,
            file_size,
            size_ok:   None,
            mtime_ok:  None,
            status,
        });
    }

    let _ = win.emit("verify-progress", VerifyProgress { fraction: 1.0, label: "Done".into() });

    let ok_count      = entries.iter().filter(|e| e.status == "ok").count();
    let missing_count = entries.iter().filter(|e| e.status == "missing").count();
    let fail_count    = total - ok_count - missing_count;

    Ok(VerifyResult {
        file_path:     file_path.display().to_string(),
        file_type:     "checksum".into(),
        algo:          ext,
        mhl_meta:      None,
        entries,
        total,
        ok_count,
        fail_count,
        missing_count,
    })
}

// ── MHL file verification ─────────────────────────────────────────────────────

struct MhlFileEntry {
    rel_path:       String,
    expected_hash:  String,
    expected_size:  u64,
    expected_mtime: Option<String>,
    algo:           HashAlgo,
}

struct MhlDoc {
    meta:    MhlMeta,
    entries: Vec<MhlFileEntry>,
    algo:    HashAlgo,   // dominant algo across all hashes
}

fn text_of(node: &roxmltree::Node) -> String {
    node.text().unwrap_or("").trim().to_string()
}

fn parse_mhl_file(path: &Path) -> io::Result<MhlDoc> {
    let text = std::fs::read_to_string(path)?;
    let doc  = roxmltree::Document::parse(&text)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    let root = doc.root_element();
    let mut meta    = MhlMeta::default();
    let mut entries = Vec::new();
    let mut doc_algo = HashAlgo::Md5;

    for child in root.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "creatorinfo" => {
                for ci in child.children().filter(|n| n.is_element()) {
                    match ci.tag_name().name() {
                        "creationdate" => meta.finish_date = text_of(&ci),
                        "tool"         => meta.creator     = text_of(&ci),
                        "location"     => meta.location    = text_of(&ci),
                        "comment"      => meta.comment     = text_of(&ci),
                        "author" => {
                            if ci.attribute("role") == Some("organization") {
                                meta.author_company = text_of(&ci);
                            } else {
                                meta.author_name = text_of(&ci);
                                if let Some(e) = ci.attribute("email") { meta.author_email = e.to_string(); }
                                if let Some(p) = ci.attribute("phone") { meta.author_phone = p.to_string(); }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "processinfo" => {
                for pi in child.children().filter(|n| n.is_element()) {
                    if pi.tag_name().name() == "process" {
                        meta.process = text_of(&pi);
                    }
                }
            }
            "hashes" => {
                for hash_el in child.children().filter(|n| n.is_element()) {
                    if hash_el.tag_name().name() != "hash" { continue; }
                    let mut entry = MhlFileEntry {
                        rel_path:       String::new(),
                        expected_hash:  String::new(),
                        expected_size:  0,
                        expected_mtime: None,
                        algo:           HashAlgo::Md5,
                    };
                    for he in hash_el.children().filter(|n| n.is_element()) {
                        match he.tag_name().name() {
                            // Spec §6.5: <path> is a direct child of <hash>,
                            // with size and lastmodificationdate as XML attributes.
                            "path" => {
                                entry.rel_path      = text_of(&he);
                                entry.expected_size = he.attribute("size")
                                    .and_then(|s| s.parse().ok()).unwrap_or(0);
                                entry.expected_mtime = he.attribute("lastmodificationdate")
                                    .map(|s| s.to_string());
                            }
                            name => {
                                let a = HashAlgo::from_mhl_element(name);
                                if a != HashAlgo::None {
                                    entry.expected_hash = text_of(&he);
                                    entry.algo = a;
                                    doc_algo = a;
                                }
                            }
                        }
                    }
                    if !entry.rel_path.is_empty() { entries.push(entry); }
                }
            }
            "references" => {
                for ref_el in child.children().filter(|n| n.is_element()) {
                    if ref_el.tag_name().name() == "hashlistreference" {
                        for re in ref_el.children().filter(|n| n.is_element()) {
                            if re.tag_name().name() == "path" {
                                meta.parent_ref = Some(text_of(&re));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Parse generation number from the filename prefix
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        meta.generation = name.split('_').next()
            .and_then(|g| g.parse::<u32>().ok())
            .unwrap_or(1);
    }
    meta.hash_algo = doc_algo.mhl_element().unwrap_or("").to_string();

    Ok(MhlDoc { meta, entries, algo: doc_algo })
}

fn run_mhl_verify(file_path: &Path, win: &tauri::WebviewWindow, pc: &Arc<PauseCancel>) -> io::Result<VerifyResult> {
    let doc = parse_mhl_file(file_path)?;
    // Files are resolved relative to the MHL's parent-parent directory
    // (MHL lives in `{dst}/ascmhl/file.mhl`, files live in `{dst}/`)
    let root = file_path.parent()         // ascmhl/
        .and_then(|p| p.parent())         // dst/
        .unwrap_or(Path::new("."));

    let total = doc.entries.len();
    if total == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidData,
            "No usable entry in this MHL (empty, or not an ASC MHL v2.0 file)."));
    }
    let mut entries = Vec::with_capacity(total);

    for (i, raw) in doc.entries.iter().enumerate() {
        pc.wait_if_paused().map_err(|_| io::Error::new(io::ErrorKind::Interrupted, "cancelled"))?;

        let _ = win.emit("verify-progress", VerifyProgress {
            fraction: i as f64 / total.max(1) as f64,
            label:    format!("Verifying {} / {}", i + 1, total),
        });

        let abs = resolve_entry(root, &raw.rel_path);
        let meta_result = match abs.as_deref() {
            Some(p) => std::fs::metadata(p),
            None    => Err(io::Error::new(io::ErrorKind::PermissionDenied, REFUSED_PATH)),
        };
        let file_size   = meta_result.as_ref().map(|m| m.len()).unwrap_or(0);

        let size_ok = if raw.expected_size > 0 {
            Some(file_size == raw.expected_size)
        } else {
            None
        };

        // mtime comparison (string-level, both in ISO 8601 UTC)
        let mtime_ok = raw.expected_mtime.as_deref().map(|exp| {
            meta_result.as_ref().ok()
                .and_then(|m| m.modified().ok())
                .map(|t| {
                    let dt: chrono::DateTime<chrono::Utc> = t.into();
                    let actual = dt.format("%Y-%m-%dT%H:%M:%SZ").to_string();
                    actual == exp
                })
                .unwrap_or(false)
        });

        let (computed, status) = if abs.is_none() {
            (String::new(), REFUSED_PATH.to_string())
        } else if meta_result.is_err() {
            (String::new(), "missing".to_string())
        } else {
            match crate::copy_engine::hash_file_verified(abs.as_deref().unwrap(), raw.algo, pc) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => return Err(e),
                Ok(hash) => {
                    let ok = hash.eq_ignore_ascii_case(&raw.expected_hash);
                    (hash, if ok { "ok" } else { "mismatch" }.to_string())
                }
                Err(e) => (String::new(), format!("error: {}", e)),
            }
        };

        let _ = win.emit("verify-entry", VerifyEntryEvent {
            index: i, computed: computed.clone(), status: status.clone(),
            file_size, size_ok, mtime_ok,
        });
        entries.push(VerifyEntry {
            rel_path:  raw.rel_path.clone(),
            expected:  raw.expected_hash.clone(),
            computed,
            file_size,
            size_ok,
            mtime_ok,
            status,
        });
    }

    let _ = win.emit("verify-progress", VerifyProgress { fraction: 1.0, label: "Done".into() });

    let ok_count      = entries.iter().filter(|e| e.status == "ok").count();
    let missing_count = entries.iter().filter(|e| e.status == "missing").count();
    let fail_count    = total - ok_count - missing_count;

    let algo_str = doc.algo.mhl_element().unwrap_or("").to_string();

    Ok(VerifyResult {
        file_path:     file_path.display().to_string(),
        file_type:     "mhl".into(),
        algo:          algo_str,
        mhl_meta:      Some(doc.meta),
        entries,
        total,
        ok_count,
        fail_count,
        missing_count,
    })
}

// ── Post-verification MHL ─────────────────────────────────────────────────────

/// Write a new MHL in the same `ascmhl/` directory as the verified MHL,
/// generation N+1, with a `<references>` block pointing to the verified MHL.
///
/// Per the ASC MHL v2.0 schema, `<process>` is constrained to
/// `in-place | transfer | flatten` — there is no `verify` value. A verification
/// re-hashes files where they already sit, so the correct process is `in-place`;
/// the verification outcome is recorded per file via the hash `action` attribute
/// (`verified` / `failed`, both defined by the schema's `ActionAttributeType`).
pub fn write_post_verify_mhl(
    verified_mhl_path: &Path,
    result:            &VerifyResult,
    settings:          &crate::settings::Settings,
) -> io::Result<PathBuf> {
    use crate::mhl_report;

    let algo = HashAlgo::from_mhl_element(&result.algo);
    let mhl_elem = match algo.mhl_element() {
        Some(e) => e,
        None => return Err(io::Error::new(io::ErrorKind::InvalidInput, "no MHL element for algo")),
    };

    // Destination root: parent of ascmhl/
    let dst = verified_mhl_path.parent()    // ascmhl/
        .and_then(|p| p.parent())           // dst/
        .unwrap_or(Path::new("."));

    // Build file entries from verification result (use computed hash + status for action attr)
    let entries: Vec<(String, String, String)> = result.entries.iter()
        .filter(|e| !e.computed.is_empty())
        .map(|e| (e.rel_path.clone(), e.computed.clone(), e.status.clone()))
        .collect();

    // Same writer as the copy engine: next generation in {root}/ascmhl/, chain
    // file updated. Spec-valid ProcessType values are in-place | transfer |
    // flatten; a verify pass hashes files in place. The per-file action
    // carries the verified / failed outcome.
    let mhl_entries: Vec<mhl_report::MhlEntry> = entries.into_iter()
        .map(|(rel, hash, status)| mhl_report::MhlEntry {
            rel, hash, action: if status == "ok" { "verified" } else { "failed" },
        })
        .collect();
    mhl_report::write_generation(dst, mhl_elem, "in-place", &mhl_entries,
                                 "", "", settings, chrono::Utc::now())
}

// ── HTML report ───────────────────────────────────────────────────────────────

pub fn write_html_report(result: &VerifyResult, out_path: &Path) -> io::Result<()> {
    use std::io::Write;
    use chrono::Local;

    let mut f = std::fs::File::create(out_path)?;

    let he = |s: &str| s.replace('&',"&amp;").replace('<',"&lt;").replace('>',"&gt;");
    let date_str = Local::now().format("%Y-%m-%d  %H:%M:%S").to_string();

    let title = format!("Bartleby — Verification Report");
    let algo_upper = result.algo.to_uppercase();
    let is_mhl = result.file_type == "mhl";

    // Status badge helpers
    let badge = |status: &str| match status {
        "ok"       => "<span class=\"badge ok\">✓ OK</span>",
        "mismatch" => "<span class=\"badge fail\">✗ MISMATCH</span>",
        "missing"  => "<span class=\"badge miss\">⚠ MISSING</span>",
        _          => "<span class=\"badge err\">! ERROR</span>",
    };

    let tick = |b: Option<bool>| match b {
        Some(true)  => "<span class=\"ok\">✓</span>",
        Some(false) => "<span class=\"fail\">✗</span>",
        None        => "—",
    };

    write!(f, r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8"/>
<title>{title}</title>
<style>
:root{{font-family:system-ui,sans-serif;font-size:13px;color:#1a1a1a;background:#f7f7f8}}
body{{margin:0;padding:24px 32px}}
h1{{font-size:22px;font-weight:700;margin:0 0 4px}}
.subtitle{{color:#666;margin:0 0 20px;font-size:12px}}
.meta-grid{{display:grid;grid-template-columns:repeat(auto-fill,minmax(260px,1fr));gap:10px;margin-bottom:20px}}
.meta-card{{background:#fff;border:1px solid #e0e0e0;border-radius:6px;padding:12px 16px}}
.meta-card h3{{margin:0 0 8px;font-size:11px;text-transform:uppercase;letter-spacing:.06em;color:#888}}
.meta-card p{{margin:3px 0;font-size:12px}}
.summary{{display:flex;gap:12px;margin-bottom:20px}}
.stat{{flex:1;background:#fff;border:1px solid #e0e0e0;border-radius:6px;padding:12px 16px;text-align:center}}
.stat .n{{font-size:28px;font-weight:700;line-height:1}}
.stat .l{{font-size:11px;color:#888;margin-top:4px}}
.n.ok{{color:#2a9d53}}.n.fail{{color:#d63a2b}}.n.miss{{color:#c07020}}
table{{width:100%;border-collapse:collapse;background:#fff;border:1px solid #e0e0e0;border-radius:6px;overflow:hidden}}
th{{background:#f0f0f2;font-size:11px;text-transform:uppercase;letter-spacing:.05em;padding:8px 12px;text-align:left;border-bottom:1px solid #e0e0e0}}
td{{padding:6px 12px;border-bottom:1px solid #f0f0f0;font-size:12px;vertical-align:top}}
tr:last-child td{{border-bottom:none}}
tr.row-ok{{background:#f6fff8}}
tr.row-fail{{background:#fff5f5}}
tr.row-miss{{background:#fffbf0}}
.path{{font-family:monospace;word-break:break-all}}
.hash{{font-family:monospace;font-size:11px;color:#555;word-break:break-all}}
.badge{{display:inline-block;padding:2px 7px;border-radius:3px;font-size:11px;font-weight:600}}
.badge.ok{{background:#d4f0dc;color:#1a7a3e}}
.badge.fail{{background:#fde8e6;color:#c0271a}}
.badge.miss{{background:#fef3e2;color:#a05a10}}
.badge.err{{background:#ede8ff;color:#5a2d91}}
.ok{{color:#2a9d53;font-weight:600}}.fail{{color:#d63a2b;font-weight:600}}
.footer{{margin-top:20px;color:#aaa;font-size:11px}}
</style>
</head>
<body>
<h1>{title}</h1>
<p class="subtitle">Generated {date_str} — Source: <code>{src}</code></p>
"#, title=he(&title), date_str=he(&date_str), src=he(&result.file_path))?;

    // MHL metadata card
    if let Some(ref meta) = result.mhl_meta {
        writeln!(f, "<div class=\"meta-grid\">")?;
        writeln!(f, "<div class=\"meta-card\"><h3>MHL Info</h3>")?;
        if !meta.creator.is_empty()     { writeln!(f, "<p><b>Creator:</b> {}</p>", he(&meta.creator))?; }
        if !meta.finish_date.is_empty() { writeln!(f, "<p><b>Date:</b> {}</p>", he(&meta.finish_date))?; }
        if !meta.process.is_empty()     { writeln!(f, "<p><b>Process:</b> {}</p>", he(&meta.process))?; }
        writeln!(f, "<p><b>Generation:</b> {:04}</p>", meta.generation)?;
        writeln!(f, "</div>")?;
        if !meta.author_company.is_empty() || !meta.author_name.is_empty()
            || !meta.author_email.is_empty() || !meta.author_phone.is_empty() {
            writeln!(f, "<div class=\"meta-card\"><h3>Author</h3>")?;
            if !meta.author_company.is_empty() { writeln!(f, "<p>{}</p>", he(&meta.author_company))?; }
            if !meta.author_name.is_empty()    { writeln!(f, "<p>{}</p>", he(&meta.author_name))?; }
            if !meta.author_email.is_empty()   { writeln!(f, "<p style=\"color:#888\">{}</p>", he(&meta.author_email))?; }
            if !meta.author_phone.is_empty()   { writeln!(f, "<p style=\"color:#888\">{}</p>", he(&meta.author_phone))?; }
            writeln!(f, "</div>")?;
        }
        if !meta.location.is_empty() || !meta.comment.is_empty() {
            writeln!(f, "<div class=\"meta-card\"><h3>Notes</h3>")?;
            if !meta.location.is_empty() { writeln!(f, "<p><b>Location:</b> {}</p>", he(&meta.location))?; }
            if !meta.comment.is_empty()  { writeln!(f, "<p><b>Comment:</b> {}</p>",  he(&meta.comment))?; }
            writeln!(f, "</div>")?;
        }
        if let Some(ref pr) = meta.parent_ref {
            writeln!(f, "<div class=\"meta-card\"><h3>Parent Reference</h3><p class=\"hash\">{}</p></div>", he(pr))?;
        }
        writeln!(f, "</div>")?;
    }

    // Summary stats
    writeln!(f, "<div class=\"summary\">")?;
    writeln!(f, "<div class=\"stat\"><div class=\"n\">{}</div><div class=\"l\">Total files</div></div>", result.total)?;
    writeln!(f, "<div class=\"stat\"><div class=\"n ok\">{}</div><div class=\"l\">Passed</div></div>", result.ok_count)?;
    writeln!(f, "<div class=\"stat\"><div class=\"n fail\">{}</div><div class=\"l\">Failed</div></div>", result.fail_count)?;
    writeln!(f, "<div class=\"stat\"><div class=\"n miss\">{}</div><div class=\"l\">Missing</div></div>", result.missing_count)?;
    writeln!(f, "</div>")?;

    // Results table
    writeln!(f, "<table>")?;
    write!(f, "<thead><tr><th>File</th><th>Status</th><th>Expected {algo}</th><th>Computed {algo}</th>",
        algo=he(&algo_upper))?;
    if is_mhl {
        write!(f, "<th>Size</th><th>Mtime</th>")?;
    }
    writeln!(f, "</tr></thead><tbody>")?;

    for e in &result.entries {
        let row_cls = match e.status.as_str() {
            "ok"       => "row-ok",
            "mismatch" => "row-fail",
            "missing"  => "row-miss",
            _          => "row-fail",
        };
        write!(f, "<tr class=\"{}\">", row_cls)?;
        write!(f, "<td class=\"path\">{}</td>", he(&e.rel_path))?;
        write!(f, "<td>{}</td>", badge(&e.status))?;
        write!(f, "<td class=\"hash\">{}</td>", he(&e.expected))?;
        write!(f, "<td class=\"hash\">{}</td>", he(&e.computed))?;
        if is_mhl {
            write!(f, "<td>{}</td><td>{}</td>", tick(e.size_ok), tick(e.mtime_ok))?;
        }
        writeln!(f, "</tr>")?;
    }

    writeln!(f, "</tbody></table>")?;
    writeln!(f, "<p class=\"footer\">Bartleby {} — {}</p>", crate::VERSION, date_str)?;
    writeln!(f, "</body></html>")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(l: &str) -> Option<(String, String)> {
        parse_checksum_line(l).map(|e| (e.expected, e.rel_path))
    }

    #[test]
    fn checksum_line_formats() {
        let h = "d41d8cd98f00b204e9800998ecf8427e";
        assert_eq!(p(&format!("{h}  A001/clip.mov")), Some((h.into(), "A001/clip.mov".into())));
        assert_eq!(p(&format!("{h} *A001/clip.mov")), Some((h.into(), "A001/clip.mov".into())));
        assert_eq!(p(&format!("{h}  name  with  spaces.mov")), Some((h.into(), "name  with  spaces.mov".into())));
        assert_eq!(p(&format!("{h}  trailing.mov \r")), Some((h.into(), "trailing.mov ".into())));
        assert_eq!(p(&format!("MD5 (A001/clip (1).mov) = {h}")), Some((h.into(), "A001/clip (1).mov".into())));
        assert_eq!(p(&format!("\\{h}  a\\nb\\\\c")), Some((h.into(), "a\nb\\c".into())));
        assert_eq!(p("XXH3_0123456789abcdef  clip.mov"), Some(("0123456789abcdef".into(), "clip.mov".into())));
        assert_eq!(p("# FAILED: x"), None);
        assert_eq!(p("   "), None);
    }

    #[test]
    fn entries_cannot_leave_the_folder() {
        let root = Path::new("/media/card");
        assert!(resolve_entry(root, "A001/clip.mov").is_some());
        assert!(resolve_entry(root, "../etc/passwd").is_none());
        assert!(resolve_entry(root, "A001/../../x").is_none());
        assert!(resolve_entry(root, "/etc/passwd").is_none());
    }
}
