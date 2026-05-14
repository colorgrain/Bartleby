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
use serde::{Serialize, Deserialize};
use tauri::Emitter;
use crate::copy_engine::HashAlgo;

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
    pub creator:      String,
    pub finish_date:  String,
    pub author_name:  String,
    pub author_email: String,
    pub location:     String,
    pub comment:      String,
    pub process:      String,       // "copy" | "verify" | ""
    pub generation:   u32,
    pub parent_ref:   Option<String>,
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

#[derive(Serialize, Clone)]
struct VerifyProgress {
    fraction: f64,
    label:    String,
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Spawn a background thread that verifies `file_path` and emits Tauri events
/// to `win`:
/// - `"verify-progress"` → `VerifyProgress` (fraction 0.0–1.0, label)
/// - `"verify-done"`     → `VerifyResult`
/// - `"verify-error"`    → String error message
pub fn run(file_path: PathBuf, win: tauri::WebviewWindow) {
    std::thread::spawn(move || {
        let ext = file_path.extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        let result = if ext == "mhl" {
            run_mhl_verify(&file_path, &win)
        } else {
            run_checksum_verify(&file_path, &win)
        };

        match result {
            Ok(r)  => { let _ = win.emit("verify-done",  r); }
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
    let mut entries = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') { continue; }
        // Formats: "<hash>  <path>"  |  "<hash> *<path>"  |  "<hash> <path>"
        let (hash, rel) = if let Some(i) = line.find("  ") {
            (&line[..i], line[i + 2..].trim())
        } else if let Some(i) = line.find(" *") {
            (&line[..i], &line[i + 2..])
        } else if let Some(i) = line.find(' ') {
            (&line[..i], line[i + 1..].trim())
        } else {
            continue;
        };
        if hash.is_empty() || rel.is_empty() { continue; }
        entries.push(RawEntry { rel_path: rel.to_string(), expected: hash.to_string() });
    }
    Ok(entries)
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

fn run_checksum_verify(file_path: &Path, win: &tauri::WebviewWindow) -> io::Result<VerifyResult> {
    let ext  = file_path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let algo = algo_from_ext(&ext);
    if algo == HashAlgo::None {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            format!("Unrecognised checksum extension: .{}", ext)));
    }

    let raw = parse_checksum_file(file_path)?;
    // Files are resolved relative to the checksum file's parent directory
    let root = file_path.parent().unwrap_or(Path::new("."));
    let total = raw.len();
    let mut entries = Vec::with_capacity(total);

    for (i, raw_e) in raw.iter().enumerate() {
        let _ = win.emit("verify-progress", VerifyProgress {
            fraction: i as f64 / total.max(1) as f64,
            label:    format!("Verifying {} / {}", i + 1, total),
        });

        let rel_native = raw_e.rel_path.replace('/', std::path::MAIN_SEPARATOR_STR);
        let abs = root.join(&rel_native);

        let (computed, file_size, status) = match crate::copy_engine::hash_file_path(&abs, algo) {
            Ok(hash) => {
                let size = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
                let ok   = hash.eq_ignore_ascii_case(&raw_e.expected);
                (hash, size, if ok { "ok" } else { "mismatch" }.to_string())
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                (String::new(), 0, "missing".to_string())
            }
            Err(e) => (String::new(), 0, format!("error: {}", e)),
        };

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
                        "name"       => meta.creator     = text_of(&ci),
                        "finishdate" => meta.finish_date = text_of(&ci),
                        "location"   => meta.location    = text_of(&ci),
                        "comment"    => meta.comment     = text_of(&ci),
                        "author"     => {
                            for a in ci.children().filter(|n| n.is_element()) {
                                match a.tag_name().name() {
                                    "name"  => meta.author_name  = text_of(&a),
                                    "email" => meta.author_email = text_of(&a),
                                    _       => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            "processinfo" => {
                for pi in child.children().filter(|n| n.is_element()) {
                    if pi.tag_name().name() == "process" { meta.process = text_of(&pi); }
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
                            "file" => {
                                for fe in he.children().filter(|n| n.is_element()) {
                                    match fe.tag_name().name() {
                                        "path" => entry.rel_path      = text_of(&fe),
                                        "size" => entry.expected_size = text_of(&fe).parse().unwrap_or(0),
                                        _      => {}
                                    }
                                }
                            }
                            "lastmodificationdate" => {
                                entry.expected_mtime = Some(text_of(&he));
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
                    if ref_el.tag_name().name() == "reference" {
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

    Ok(MhlDoc { meta, entries, algo: doc_algo })
}

fn run_mhl_verify(file_path: &Path, win: &tauri::WebviewWindow) -> io::Result<VerifyResult> {
    let doc = parse_mhl_file(file_path)?;
    // Files are resolved relative to the MHL's parent-parent directory
    // (MHL lives in `{dst}/ascmhl/file.mhl`, files live in `{dst}/`)
    let root = file_path.parent()         // ascmhl/
        .and_then(|p| p.parent())         // dst/
        .unwrap_or(Path::new("."));

    let total = doc.entries.len();
    let mut entries = Vec::with_capacity(total);

    for (i, raw) in doc.entries.iter().enumerate() {
        let _ = win.emit("verify-progress", VerifyProgress {
            fraction: i as f64 / total.max(1) as f64,
            label:    format!("Verifying {} / {}", i + 1, total),
        });

        let rel_native = raw.rel_path.replace('/', std::path::MAIN_SEPARATOR_STR);
        let abs = root.join(&rel_native);

        let meta_result = std::fs::metadata(&abs);
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

        let (computed, status) = if meta_result.is_err() {
            (String::new(), "missing".to_string())
        } else {
            match crate::copy_engine::hash_file_path(&abs, raw.algo) {
                Ok(hash) => {
                    let ok = hash.eq_ignore_ascii_case(&raw.expected_hash);
                    (hash, if ok { "ok" } else { "mismatch" }.to_string())
                }
                Err(e) => (String::new(), format!("error: {}", e)),
            }
        };

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
/// with `<process>verify</process>`, generation N+1, and a `<references>`
/// block pointing to the verified MHL.
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

    let meta = result.mhl_meta.as_ref().cloned().unwrap_or_default();

    // Destination root: parent of ascmhl/
    let dst = verified_mhl_path.parent()    // ascmhl/
        .and_then(|p| p.parent())           // dst/
        .unwrap_or(Path::new("."));

    // Build file entries from verification result (use computed hash)
    let entries: Vec<(String, String)> = result.entries.iter()
        .filter(|e| !e.computed.is_empty())
        .map(|e| (e.rel_path.clone(), e.computed.clone()))
        .collect();

    // Compute hash of the verified MHL file to put in <references>
    let ref_hash = crate::copy_engine::hash_file_path(verified_mhl_path, algo)
        .unwrap_or_default();

    let src_ref = mhl_report::MhlRef {
        path:       verified_mhl_path.display().to_string(),
        hash:       ref_hash,
        generation: meta.generation,
    };

    let new_gen = mhl_report::find_dst_mhl_for_src(dst, "")  // don't filter by src_name
        .map(|(_, g)| g + 1)
        .unwrap_or(meta.generation + 1);

    // Write via mhl_report but override the process element afterward
    // Simpler: build the XML directly for verify MHLs
    use std::io::Write;
    use chrono::{Local, Utc, DateTime};

    let now_local: DateTime<Local> = Local::now();
    let now_utc:   DateTime<Utc>   = Utc::now();
    let date_str   = now_local.format("%Y-%m-%d").to_string();
    let time_str   = now_utc.format("%H%M%SZ").to_string();
    let finish_iso = now_utc.format("%Y-%m-%dT%H:%M:%SZ").to_string();

    let dir      = dst.join("ascmhl");
    std::fs::create_dir_all(&dir)?;
    let src_name = verified_mhl_path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.split('_').nth(1))   // extract src_name from filename
        .unwrap_or("unknown");
    let filename = format!("{:04}_{}_{}_{}.mhl", new_gen, src_name, date_str, time_str);
    let out_path = dir.join(&filename);
    let mut f    = std::fs::File::create(&out_path)?;

    let xe = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
                         .replace('"', "&quot;").replace('\'', "&apos;");

    writeln!(f, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    writeln!(f, "<hashlist xmlns=\"urn:ASC:MHL:v2.0\" version=\"2.0\">")?;
    writeln!(f, "  <creatorinfo>")?;
    writeln!(f, "    <name>Bartleby {}</name>", crate::VERSION)?;
    writeln!(f, "    <finishdate>{}</finishdate>", finish_iso)?;
    writeln!(f, "    <author>")?;
    let author = if !settings.company.is_empty() && !settings.contact_name.is_empty() {
        format!("{} / {}", xe(&settings.company), xe(&settings.contact_name))
    } else if !settings.contact_name.is_empty() { xe(&settings.contact_name)
    } else if !settings.company.is_empty()      { xe(&settings.company)
    } else                                        { String::new() };
    if !author.is_empty() { writeln!(f, "      <name>{}</name>", author)?; }
    if !settings.email.is_empty() { writeln!(f, "      <email>{}</email>", xe(&settings.email))?; }
    writeln!(f, "    </author>")?;
    writeln!(f, "  </creatorinfo>")?;
    writeln!(f, "  <processinfo>")?;
    writeln!(f, "    <process>verify</process>")?;
    writeln!(f, "  </processinfo>")?;
    writeln!(f, "  <hashes>")?;
    for (rel, hash) in &entries {
        if hash.is_empty() { continue; }
        let rel_native = rel.replace('/', std::path::MAIN_SEPARATOR_STR);
        let abs = dst.join(&rel_native);
        let (size, mtime_iso) = if let Ok(m) = std::fs::metadata(&abs) {
            let mtime = m.modified().ok()
                .map(|t| { let dt: DateTime<Utc> = t.into(); dt.format("%Y-%m-%dT%H:%M:%SZ").to_string() })
                .unwrap_or_default();
            (m.len(), mtime)
        } else { (0, String::new()) };
        writeln!(f, "    <hash>")?;
        writeln!(f, "      <file><path>{}</path><size>{}</size></file>", xe(rel), size)?;
        writeln!(f, "      <{}>{}</{}>", mhl_elem, hash, mhl_elem)?;
        if !mtime_iso.is_empty() {
            writeln!(f, "      <lastmodificationdate>{}</lastmodificationdate>", mtime_iso)?;
        }
        writeln!(f, "    </hash>")?;
    }
    writeln!(f, "  </hashes>")?;
    writeln!(f, "  <references>")?;
    writeln!(f, "    <reference>")?;
    writeln!(f, "      <path>{}</path>", xe(&src_ref.path))?;
    if !src_ref.hash.is_empty() {
        writeln!(f, "      <{}>{}</{}>", mhl_elem, src_ref.hash, mhl_elem)?;
    }
    writeln!(f, "    </reference>")?;
    writeln!(f, "  </references>")?;
    writeln!(f, "</hashlist>")?;

    Ok(out_path)
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
        if !meta.author_name.is_empty() || !meta.author_email.is_empty() {
            writeln!(f, "<div class=\"meta-card\"><h3>Author</h3>")?;
            if !meta.author_name.is_empty()  { writeln!(f, "<p>{}</p>", he(&meta.author_name))?; }
            if !meta.author_email.is_empty() { writeln!(f, "<p>{}</p>", he(&meta.author_email))?; }
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
