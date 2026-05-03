//! # Module `copy_engine` — multi-destination transfer engine
//!
//! Core engine for Bartleby. Copies files from one source directory to N
//! destination directories with optional MD5 and/or XXH3 integrity verification.
//!
//! ## Three-phase architecture
//!
//! ```text
//! Phase 1 ── Kernel copy ──────────────────────────────────────────────────
//!   For each file (sequentially):                                           
//!     ┌── Copy to dst1 ──┐                                                  
//!     ├── Copy to dst2 ──┤  ← rayon parallel per destination               
//!     └── Copy to dstN ──┘                                                  
//!     sync_all(dst1, dst2, dstN)  ← fsync: data+metadata on physical disk  
//!                                                                            
//! Phase 2 ── Integrity verification ──────────────────────────────────────  
//!   For each file (sequentially):                                           
//!     ┌── hash(src)  ──┐                                                    
//!     ├── hash(dst1) ──┤  ← rayon parallel, O_DIRECT bypasses page cache   
//!     └── hash(dstN) ──┘                                                    
//!     compare: src_hash == dst1_hash == dstN_hash ?                        
//!                                                                            
//! Phase 3 ── Reports ──────────────────────────────────────────────────────  
//!   Write .md5 / .xxh3 / .csv / .pdf as requested                          
//! ```
//!
//! ## Why copy first, then hash separately?
//!
//! An earlier version hashed during the copy (read chunk → hash → write).
//! This prevented the kernel from using `copy_file_range()` (zero-copy),
//! reducing throughput from ~2 GB/s to ~100 MB/s — a 20× slowdown.
//! Separating copy and hash recovers kernel-speed copy performance.
//!
//! ## Why O_DIRECT for hashing?
//!
//! After `sync_all()`, the kernel page cache and physical disk are identical.
//! However, reading through the cache would hash RAM copies of the data.
//! O_DIRECT forces reads directly from the storage device, making the
//! verification a true end-to-end integrity check.
//!
//! ## Hash algorithms
//!
//! - **MD5** via OpenSSL EVP (Linux), CommonCrypto (macOS), CNG (Windows).
//!   System libraries use SHA-NI/AVX2 assembly: ~3–5 GB/s.
//! - **XXH3-128** via `twox-hash 2.x` with `target-cpu=native`.
//!   Pure Rust + AVX2: ~8–12 GB/s in release builds.
//! - Both can run simultaneously in a single read pass — zero extra I/O.
//!
//! ## Thread model
//!
//! `run()` blocks its OS thread for the entire transfer duration.
//! It is always called from `std::thread::spawn()` in `main.rs`, never
//! from the Tokio async executor, which would block the event loop.
//! Progress updates are sent via `mpsc::Sender<Msg>` to the Tauri frontend.
//!
//! ### Phase 3 — Reports (.md5, .xxh3, .csv, .pdf)

// libc: C standard library bindings for Unix-specific syscalls.
// Used for:
//   - O_DIRECT flag on Linux (open file bypassing the page cache)
//   - F_NOCACHE fcntl on macOS (disable unified buffer cache per-fd)
//   - posix_memalign() for 4096-byte aligned buffer allocation (O_DIRECT requirement)
// `#[cfg(unix)]` : compiled only on Unix platforms (Linux + macOS).
// On Windows, the equivalent is FILE_FLAG_NO_BUFFERING via windows-sys.
#[cfg(unix)]
use libc;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Instant;
use rayon::prelude::*;
use crate::metadata;
use crate::pdf_report;
use crate::html_report;
use crate::settings::Settings;

/// Size of each read chunk in the hash pipeline (bytes).
///
/// ## Why 4 MiB?
///
/// O_DIRECT requires the userspace buffer to be aligned to the disk sector
/// size (512 or 4096 bytes). We allocate 4 MiB chunks via `posix_memalign()`.
///
/// 4 MiB is a sweet spot:
///   - Large enough to amortise syscall overhead (few `read()` calls per file).
///   - Small enough to fit in CPU L3 cache (typical L3 = 8–32 MiB), keeping
///     hash computation cache-warm.
///   - A power of two and a multiple of 4096 — satisfies all sector alignments.
///
/// Increasing to 16 MiB gives marginal throughput improvement on NVMe but
/// uses 4× more RAM per concurrent hash operation.
const HASH_CHUNK: usize = 4 * 1024 * 1024; // 4 MiB = 4_194_304 bytes

// ── Hash result ───────────────────────────────────────────────────────────────

/// Holds the computed hash digests for one file.
///
/// Both fields are `Option<String>` because each hash type is independently
/// optional — controlled by the MD5 and XXH3 checkboxes in the UI.
///
/// `None`         = this hash type was not requested (checkbox unchecked).
/// `Some(String)` = 32-character lowercase hexadecimal digest string.
///
/// ### Why `Option` instead of an empty string for "not computed"?
/// An empty string is ambiguous — it could mean "not requested" or "hash of
/// an empty file". `Option` makes the distinction explicit and forces callers
/// to handle both cases, preventing silent bugs where a missing hash is
/// compared against another missing hash and incorrectly reported as matching.
///
/// ### Derive attributes
/// - `Clone`   : needed when passing results from the parallel rayon closure
///               back to the main thread via a `Mutex<Vec<FileHashes>>`.
/// - `Default` : `FileHashes::default()` produces `{ md5: None, xxh: None }`,
///               used as a sentinel for files that were skipped during copy.
/// - `Debug`   : enables `println!("{:?}", hashes)` for development logging.
#[derive(Clone, Default, Debug)]
struct FileHashes {
    /// MD5 digest: 32-char lowercase hex, e.g. `"d41d8cd98f00b204e9800998ecf8427e"`.
    /// `None` if MD5 was not requested by the user.
    md5: Option<String>,
    /// XXH3-128 digest: 32-char lowercase hex.
    /// `None` if XXH3 was not requested by the user.
    xxh: Option<String>,
}

impl FileHashes {
    /// Returns `true` if all hash types present in both `self` and `other`
    /// have identical digests.
    ///
    /// Only compares hash types that are `Some(…)` in **both** structs.
    /// If a hash type is `None` in either (was not requested), it is skipped.
    ///
    /// ### Pattern: `if let (Some(a), Some(b)) = (…, …)`
    /// This is a tuple destructuring `if let`. It binds `a` and `b` only when
    /// *both* Options are `Some`. If either is `None`, the block is skipped.
    /// Equivalent to:
    /// ```rust
    /// if self.md5.is_some() && other.md5.is_some() {
    ///     if self.md5.unwrap() != other.md5.unwrap() { return false; }
    /// }
    /// ```
    /// The `if let` version is more idiomatic and avoids repeated unwraps.
    fn matches(&self, other: &FileHashes) -> bool {
        if let (Some(a), Some(b)) = (&self.md5, &other.md5) {
            if a != b { return false; }
        }
        if let (Some(a), Some(b)) = (&self.xxh, &other.xxh) {
            if a != b { return false; }
        }
        true
    }

    /// Returns a compact string showing the first 8 characters of each digest.
    ///
    /// Used in UI log lines: `"✓  clip001.mxf  [md5:db4802d7 xxh:ecd0a128]"`
    ///
    /// 8 hex characters = 32 bits of the hash — enough to distinguish files
    /// visually without cluttering the log with 32-character strings.
    ///
    /// ### `&m[..8.min(m.len())]`
    /// `8.min(m.len())` : the smaller of 8 and the string length.
    /// For normal MD5/XXH3 digests `m.len()` is always 32, so this is `8`.
    /// The `.min()` is a safety guard for empty strings (e.g. hash of empty file).
    /// `&m[..8]` : a string slice of the first 8 bytes (safe for ASCII hex).
    fn short(&self) -> String {
        let mut parts = Vec::new();
        if let Some(ref m) = self.md5 {
            parts.push(format!("md5:{}", &m[..8.min(m.len())]));
        }
        if let Some(ref x) = self.xxh {
            parts.push(format!("xxh:{}", &x[..8.min(x.len())]));
        }
        // `parts.join(" ")` : ["md5:db4802d7", "xxh:ecd0a128"] → "md5:db4802d7 xxh:ecd0a128"
        // Returns an empty string if no hashes were computed.
        parts.join(" ")
    }
}

// ── IPC — inter-process communication between engine and UI ──────────────────
//
// The copy engine runs on a dedicated OS thread (spawned by main.rs) and
// communicates with the Tauri frontend via two mpsc channels:
//
//   engine → UI  :  Sender<Msg>            (progress, logs, completion)
//   UI → engine  :  Receiver<Reply>        (user decisions on conflicts)
//
// mpsc = Multiple Producer, Single Consumer. Rust's standard channel type.
// `Sender<T>` can be cloned and shared across threads; `Receiver<T>` cannot.

/// Messages sent from the copy engine to the forwarding thread in `main.rs`.
///
/// The forwarding thread receives these and emits them as Tauri events to
/// the JavaScript frontend. Using an enum (rather than separate channels)
/// preserves message ordering — all messages arrive in the order they were sent.
pub enum Msg {
    /// Progress bar update: (fraction 0.0–1.0, label string for the status bar).
    /// The fraction is clamped to 0.98 during operation and set to 1.0 on completion.
    Progress(f64, String),
    /// One log line to append to the UI log panel. Always ends with `\n`.
    Log(String),
    /// Operation complete: (success: bool, one-line summary for the status bar).
    /// After sending Done, the engine thread exits.
    Done(bool, String),
    /// Pre-copy prompt: one or more destinations already contain files.
    /// The engine blocks on `reply_rx.recv()` until the user responds.
    NonEmptyDest(Vec<String>),
    /// Pre-copy prompt: specific files already exist in a destination.
    /// Same blocking mechanism as `NonEmptyDest`.
    Conflicts(Vec<String>),
}

/// User replies sent from the UI thread back to the blocked copy engine.
///
/// Sent via `Sender<Reply>` in `main.rs` when the user clicks a dialog button.
/// The engine receives this via `reply_rx.recv()` and resumes accordingly.
pub enum Reply {
    /// Proceed — overwrite conflicting files (or ignore non-empty destinations).
    Continue,
    /// Skip conflicting files, copy everything else.
    Skip,
    /// Abort the entire operation immediately.
    Cancel,
}

/// Returns elapsed time since `start` as a timecode: `HH:MM:SS.d`.
///
/// Example output: `00:00:06.6`, `00:02:05.3`
fn ts(start: &Instant) -> String {
    let total = start.elapsed().as_secs_f64();
    let h = (total / 3600.0) as u64;
    let m = ((total % 3600.0) / 60.0) as u64;
    let s = total % 60.0;
    // {:04.1} → zero-padded to width 4 with 1 decimal place: "06.6", "00.0"
    format!("{:02}:{:02}:{:04.1}", h, m, s)
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Runs the full transfer pipeline: pre-checks → copy → verify → reports.
///
/// ## Blocking behaviour
/// This function **blocks** its calling thread for the entire duration of the
/// transfer (seconds to minutes). It must be called from a dedicated OS thread
/// spawned via `std::thread::spawn()` in `main.rs` — never from an async task
/// or the Tauri command handler, which would deadlock the event loop.
///
/// ## Parameters
/// - `src`          : absolute path to the source directory to copy.
/// - `destinations` : list of destination root directories (1 or more).
/// - `verify`       : if true, run Phase 2 (hash + comparison). Always equals
///                    `gen_md5 || gen_xxh` — pre-computed by the caller.
/// - `gen_md5`      : compute MD5, verify destination, write `.md5` file.
/// - `gen_xxh`      : compute XXH3-128, verify destination, write `.xxh3` file.
/// - `gen_csv`      : generate a `.csv` metadata table report.
/// - `gen_pdf`      : generate a `.pdf` visual report with thumbnails.
/// - `gen_html`     : generate a self-contained `.html` report with thumbnails.
/// - `settings`     : snapshot of user preferences (column flags, header text).
/// - `tx`           : channel to send `Msg` events to the Tauri forwarding thread.
/// - `reply_rx`     : channel to receive `Reply` decisions from the user.
///
/// ## Error handling
/// Errors are reported via `Msg::Log` (appended to the UI log) and
/// `Msg::Done(false, summary)` at the end. The function never panics —
/// all error paths send a Done message and return cleanly.
pub fn run(
    src:          PathBuf,
    destinations: Vec<PathBuf>,
    verify:       bool,
    gen_md5:      bool,
    gen_xxh:      bool,
    gen_csv:      bool,
    gen_pdf:      bool,
    gen_html:     bool,
    settings:     Settings,
    tx:           Sender<Msg>,
    reply_rx:     std::sync::mpsc::Receiver<Reply>,
) {
    let start = Instant::now();

    log(&tx, &format!("→  Source : {}\n", src.display()));
    for dst in &destinations { log(&tx, &format!("→  Dest   : {}\n", dst.display())); }
    if gen_md5 { log(&tx, "→  Hash   : MD5\n"); }
    if gen_xxh { log(&tx, "→  Hash   : XXH3-128\n"); }
    log(&tx, "\n");

    let src_name = src.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "source".to_string());

    // ── Scan source ───────────────────────────────────────────────────────────
    let _ = tx.send(Msg::Progress(0.0, "Scanning source directory…".into()));
    let files = match collect_files(&src) {
        Ok(f) => f,
        Err(e) => {
            log(&tx, &format!("✖  Cannot read source: {}\n", e));
            let _ = tx.send(Msg::Done(false, format!("✖  {}", e)));
            return;
        }
    };
    if files.is_empty() {
        log(&tx, "△  Source directory is empty.\n");
        let _ = tx.send(Msg::Done(false, "△  Source directory is empty.".into()));
        return;
    }
    log(&tx, &format!("◎  {} file(s) found\n\n", files.len()));

    for dst in &destinations {
        if let Err(e) = fs::create_dir_all(dst) {
            log(&tx, &format!("✖  Cannot create {}: {}\n", dst.display(), e));
            let _ = tx.send(Msg::Done(false, format!("✖  {}", e)));
            return;
        }
    }

    // ── Pre-copy checks ───────────────────────────────────────────────────────
    let non_empty: Vec<String> = destinations.iter()
        .filter(|d| fs::read_dir(d).map(|mut r| r.next().is_some()).unwrap_or(false))
        .map(|d| d.to_string_lossy().to_string())
        .collect();
    if !non_empty.is_empty() {
        let _ = tx.send(Msg::NonEmptyDest(non_empty));
        match reply_rx.recv().unwrap_or(Reply::Cancel) {
            Reply::Cancel => {
                log(&tx, "✖  Cancelled.\n");
                let _ = tx.send(Msg::Done(false, "Cancelled.".into()));
                return;
            }
            _ => {}
        }
    }

    let conflict_rels: Vec<String> = files.iter().filter_map(|p| {
        let rel = p.strip_prefix(&src).unwrap_or(p);
        if destinations.iter().any(|d| d.join(rel).exists()) {
            Some(rel.to_string_lossy().replace('\\', "/"))
        } else { None }
    }).collect();

    let mut skip_set: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    if !conflict_rels.is_empty() {
        let _ = tx.send(Msg::Conflicts(conflict_rels.clone()));
        match reply_rx.recv().unwrap_or(Reply::Cancel) {
            Reply::Cancel => {
                log(&tx, "✖  Cancelled.\n");
                let _ = tx.send(Msg::Done(false, "Cancelled.".into()));
                return;
            }
            Reply::Skip => {
                for r in &conflict_rels { skip_set.insert(r.clone()); }
                log(&tx, &format!("△  {} file(s) will be skipped.\n", skip_set.len()));
            }
            Reply::Continue => { log(&tx, "△  Conflicting files will be overwritten.\n"); }
        }
    }

    // ── Progress tracking setup ───────────────────────────────────────────────
    let file_sizes: Vec<u64> = files.iter()
        .map(|p| fs::metadata(p).map(|m| m.len()).unwrap_or(0))
        .collect();
    let total_bytes: u64 = file_sizes.iter().sum();
    let n_dst = destinations.len() as u64;
    let grand_total = total_bytes * n_dst
        + if verify { total_bytes * (1 + n_dst) } else { 0 };
    let mut bytes_done: u64 = 0;

    // Phase 1 — separate read (source) and write (destinations) byte counters
    let mut p1_src_bytes: u64 = 0;
    let mut p1_src_snap:  u64 = 0;
    let mut p1_src_t           = Instant::now();
    let mut p1_dst_bytes: u64 = 0;
    let mut p1_dst_snap:  u64 = 0;
    let mut p1_dst_t           = Instant::now();

    // Phase 2 — aggregate verify read counter
    let mut p2_bytes: u64 = 0;
    let mut p2_snap:  u64 = 0;
    let mut p2_t           = Instant::now();

    // ══════════════════════════════════════════════════════════════════════════
    // PHASE 1 — Kernel copy, file by file, destinations in parallel
    //
    // Files are processed sequentially to avoid read-head contention on HDD.
    // For each file, all destination copies run in parallel via rayon —
    // each destination gets its own thread. Efficient when destinations are
    // on separate buses (USB, Thunderbolt, NVMe).
    //
    // After each file: sync_all() flushes data + metadata to physical storage.
    // This is the guarantee that what we later hash is what is on disk.
    // ══════════════════════════════════════════════════════════════════════════
    log(&tx, &format!("── Phase 1 — Copy {} ──────────────────────\n", ts(&start)));

    let mut copied: Vec<(PathBuf, String)> = Vec::new();
    let mut copy_errors = 0usize;

    for (idx, src_path) in files.iter().enumerate() {
        let rel     = src_path.strip_prefix(&src).unwrap_or(src_path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let fsize   = file_sizes[idx];

        let pct = if grand_total > 0 {
            (bytes_done as f64 / grand_total as f64).min(0.98)
        } else { 0.0 };
        let rs = fmt_speed(speed_bps(p1_src_bytes, &mut p1_src_snap, &mut p1_src_t, &start));
        let ws = fmt_speed(speed_bps(p1_dst_bytes, &mut p1_dst_snap, &mut p1_dst_t, &start));
        let _ = tx.send(Msg::Progress(pct, format!("Copying {} — R: {}  W: {}", rel_str, rs, ws)));

        if skip_set.contains(&rel_str) {
            // This file was in the conflict set and the user chose "Skip".
            // We do NOT re-copy it, but we DO add it to `copied` so that:
            //   1. Phase 2 still verifies the existing destination copy against src.
            //   2. It appears in the CSV and PDF reports.
            //   3. Its hash appears in the .md5 / .xxh3 checksum files.
            //
            // This is the correct DIT workflow: "I already have this file on disk,
            // please verify it matches the source without copying again."
            log(&tx, &format!("  ↷  skipped copy (already exists): {}\n", rel_str));
            copied.push((src_path.clone(), rel_str.clone()));
            bytes_done += fsize * n_dst;
            // `continue` : skip the copy block below, move to the next file.
            continue;
        }

        let dst_paths: Vec<PathBuf> = destinations.iter()
            .map(|d| d.join(rel)).collect();

        // Create subdirectories in all destinations
        let mut dir_ok = true;
        for dst_path in &dst_paths {
            if let Some(parent) = dst_path.parent() {
                if fs::create_dir_all(parent).is_err() { dir_ok = false; }
            }
        }
        if !dir_ok { copy_errors += 1; continue; }

        // ── 1a. Copy to all destinations in parallel ──────────────────────────
        //
        // `dst_paths.par_iter()` : rayon parallel iterator — one OS thread per
        // destination. Each thread calls `fs::copy()` independently.
        //
        // `std::fs::copy()` on Linux uses the `copy_file_range()` syscall:
        //   - Data moves entirely within the kernel — never touches userspace RAM.
        //   - Typically 2–3× faster than a userspace read/write loop.
        //   - The kernel can use DMA (Direct Memory Access) to copy between
        //     page cache entries without involving the CPU at all.
        //
        // On macOS: `copyfile(COPYFILE_ALL)` — similar zero-copy semantics.
        // On Windows: `CopyFileEx()` — Windows cache manager handles the copy.
        //
        // `Vec<io::Result<u64>>` : each element is Ok(bytes_copied) or Err(e).
        // `.collect()` gathers all results in original order (rayon guarantee).
        // On macOS: copyfile(). On Windows: CopyFileEx().
        let copy_results: Vec<io::Result<u64>> = dst_paths.par_iter()
            .map(|dst_path| fs::copy(src_path, dst_path))
            .collect();

        let mut any_error = false;
        for (i, res) in copy_results.iter().enumerate() {
            if let Err(e) = res {
                log(&tx, &format!("  ✖  → {}: {}\n", destinations[i].display(), e));
                any_error = true;
            }
        }
        if any_error { copy_errors += 1; continue; }

        // sync_all() on each destination — calls fsync() at OS level.
        // Guarantees data AND metadata (timestamps, permissions) are physically
        // written to storage before we proceed to hash verification.
        // Runs in parallel across destinations.
        let sync_results: Vec<io::Result<()>> = dst_paths.par_iter()
            .map(|dst_path| fs::OpenOptions::new().write(true).open(dst_path)?.sync_all())
            .collect();

        for (i, res) in sync_results.iter().enumerate() {
            if let Err(e) = res {
                log(&tx, &format!("  ✖  sync {}: {}\n", destinations[i].display(), e));
                copy_errors += 1;
            }
        }

        bytes_done   += fsize * n_dst;
        p1_src_bytes += fsize;
        p1_dst_bytes += fsize * n_dst;
        copied.push((src_path.clone(), rel_str.clone()));
        log(&tx, &format!("  ✓  {}\n", rel_str));
    }

    log(&tx, &format!("\n── Phase 1 complete {} ─────────────────────\n", ts(&start)));

    // ── Metadata extraction (only when CSV, PDF, or HTML is requested) ───────
    if gen_csv || gen_pdf || gen_html {
        log(&tx, &format!("\n── Metadata {} ─────────────────────────────\n", ts(&start)));
    }
    let meta_entries: Vec<(String, metadata::FileMeta)> = if gen_csv || gen_pdf || gen_html {
        copied.par_iter()
            .map(|(src_path, rel_str)| (rel_str.clone(), metadata::extract(src_path)))
            .collect()
    } else {
        copied.iter()
            .map(|(src_path, rel_str)| {
                let name = src_path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                (rel_str.clone(), metadata::FileMeta { name, ..Default::default() })
            })
            .collect()
    };

    // ── Copy-only path ────────────────────────────────────────────────────────
    if !verify {
        let no_hashes: Vec<(FileHashes, bool)> =
            copied.iter().map(|_| (FileHashes::default(), true)).collect();
        generate_reports(&tx, &destinations, &src_name, &src,
                         &meta_entries, &no_hashes,
                         gen_csv, gen_pdf, gen_html, gen_md5, gen_xxh, false, &settings);
        let summary = format!("✓  {} file(s) copied to {} destination(s) — no verification",
            copied.len(), destinations.len());
        log(&tx, &format!("\n{}\n", summary));
        let _ = tx.send(Msg::Progress(1.0, "Done".into()));
        let _ = tx.send(Msg::Done(true, summary));
        return;
    }

    // ══════════════════════════════════════════════════════════════════════════
    // PHASE 2 — Hash from physical storage (bypassing OS page cache)
    //
    // Reads use O_DIRECT on Linux, F_NOCACHE on macOS, FILE_FLAG_NO_BUFFERING
    // on Windows. This ensures we hash what is physically on disk, not cached RAM.
    //
    // For each file: source + all destinations are hashed in parallel.
    // MD5 and XXH3 are computed simultaneously in a single read pass.
    // ══════════════════════════════════════════════════════════════════════════
    log(&tx, &format!("\n── Phase 2 — Verification {} ──────────────\n", ts(&start)));

    let n_files = copied.len();
    let mut results: Vec<(FileHashes, bool)> =
        vec![(FileHashes::default(), true); n_files];
    let mut verify_errors = 0usize;

    for (file_idx, (src_path, rel_str)) in copied.iter().enumerate() {
        let fsize = fs::metadata(src_path).map(|m| m.len()).unwrap_or(0);

        let pct = if grand_total > 0 {
            (bytes_done as f64 / grand_total as f64).min(0.98)
        } else { 0.0 };
        let vs = fmt_speed(speed_bps(p2_bytes, &mut p2_snap, &mut p2_t, &start));
        let _ = tx.send(Msg::Progress(pct,
            format!("Verifying {}/{} — {} — R: {}", file_idx + 1, n_files, rel_str, vs)));

        // Build the list of paths to hash: [source, destination1, destination2, …]
        // `vec![src_path.clone()]` initialises a Vec with one element (the source).
        // We then append each destination path, reconstructed from the relative path.
        //
        // `rel_str.replace('/', MAIN_SEPARATOR_STR)` : on Windows, convert Unix-style
        // forward slashes to backslashes. MAIN_SEPARATOR_STR is "/" on Unix, "\\" on Windows.
        let mut all_paths: Vec<PathBuf> = vec![src_path.clone()];
        for dst in &destinations {
            all_paths.push(dst.join(
                rel_str.replace('/', std::path::MAIN_SEPARATOR_STR)));
        }

        // Hash all paths in parallel via rayon.
        //
        // `all_paths.par_iter()` : rayon distributes the hash operations across
        // CPU cores. For 1 source + 2 destinations, 3 threads hash simultaneously.
        //
        // Each rayon closure calls `hash_direct()` which:
        //   1. Opens the file with O_DIRECT (bypasses page cache → reads from disk)
        //   2. Spawns an internal reader thread (pipeline: read+hash overlap)
        //   3. Returns the computed FileHashes
        //
        // `Vec<io::Result<FileHashes>>` : one result per path, in original order.
        // rayon guarantees result order matches input order despite parallel execution.
        let hash_results: Vec<io::Result<FileHashes>> = all_paths.par_iter()
            .map(|path| hash_direct(path, gen_md5, gen_xxh))
            .collect();

        let mut read_error = false;
        for (i, res) in hash_results.iter().enumerate() {
            if let Err(e) = res {
                let label = if i == 0 { "source".to_string() }
                            else { destinations[i-1].display().to_string() };
                log(&tx, &format!("  ✖  read error {} ({}): {}\n", rel_str, label, e));
                read_error = true;
            }
        }
        if read_error {
            verify_errors += 1;
            results[file_idx].1 = false;
            bytes_done += fsize * (1 + n_dst);
            continue;
        }

        let hashes: Vec<FileHashes> = hash_results.into_iter()
            .map(|r| r.unwrap()).collect();
        let src_hash = &hashes[0];

        let mut mismatch = false;
        for (i, dst_hash) in hashes[1..].iter().enumerate() {
            if !src_hash.matches(dst_hash) {
                log(&tx, &format!(
                    "  ✖  MISMATCH — {}\n     src: {}\n     dst[{}]: {}\n",
                    rel_str, src_hash.short(), i + 1, dst_hash.short()));
                mismatch = true;
            }
        }

        if mismatch {
            verify_errors += 1;
            results[file_idx] = (src_hash.clone(), false);
        } else {
            log(&tx, &format!("  ✓  {}  [{}]\n", rel_str, src_hash.short()));
            results[file_idx] = (src_hash.clone(), true);
        }

        bytes_done += fsize * (1 + n_dst);
        p2_bytes   += fsize * (1 + n_dst);
    }

    log(&tx, &format!("\n── Phase 2 complete {} ─────────────────────\n", ts(&start)));

    // ── Phase 3: reports ──────────────────────────────────────────────────────
    generate_reports(&tx, &destinations, &src_name, &src,
                     &meta_entries, &results,
                     gen_csv, gen_pdf, gen_html, gen_md5, gen_xxh, true, &settings);

    if verify_errors > 0 {
        log(&tx, "\n━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
        log(&tx, &format!("⚠  {} file(s) failed verification:\n", verify_errors));
        for (i, (_, rel)) in copied.iter().enumerate() {
            if !results[i].1 { log(&tx, &format!("   ✖  {}\n", rel)); }
        }
        log(&tx, "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━\n");
    }

    let total_errors = copy_errors + verify_errors;
    let _ = tx.send(Msg::Progress(1.0, "Done".into()));
    let hash_label = match (gen_md5, gen_xxh) {
        (true,  true)  => " — MD5 + XXH3 verified",
        (true,  false) => " — MD5 verified",
        (false, true)  => " — XXH3 verified",
        _              => "",
    };
    if total_errors == 0 {
        let summary = format!("✓  {} file(s) — {} destination(s){}",
            copied.len(), destinations.len(), hash_label);
        log(&tx, &format!("\n{}\n", summary));
        let _ = tx.send(Msg::Done(true, summary));
    } else {
        let ok = copied.len().saturating_sub(verify_errors);
        let summary = format!("△  {} error(s) — {}/{} file(s) OK",
            total_errors, ok, copied.len());
        log(&tx, &format!("\n{}\n", summary));
        let _ = tx.send(Msg::Done(false, summary));
    }
}

// ── Direct I/O hash (bypasses OS page cache) ──────────────────────────────────

/// Opens a file with cache-bypassing flags and hashes its content.
///
/// ## Why bypass the cache?
///
/// After sync_all(), the page cache == physical disk by definition.
/// However, reading through the cache means we might be comparing
/// two identical RAM copies rather than verifying what is on disk.
/// Direct I/O forces the read to come from the storage device itself,
/// making the verification a true end-to-end integrity check.
///
/// ## Platform implementations
///
/// ### Linux — O_DIRECT
/// O_DIRECT bypasses the page cache entirely. The kernel transfers data
/// directly between the storage device and the userspace buffer.
/// Requirements: buffer must be aligned to the sector size (512 or 4096 bytes).
/// We use `aligned_buf()` to guarantee 4096-byte alignment.
///
/// ### macOS — F_NOCACHE
/// macOS does not support O_DIRECT on most filesystems (APFS, HFS+).
/// Instead, `fcntl(F_NOCACHE, 1)` disables the unified buffer cache (UBC)
/// for this file descriptor. Equivalent effect: reads bypass the cache.
/// No alignment requirement — the standard read() API is used.
///
/// ### Windows — FILE_FLAG_NO_BUFFERING
/// Opens the file with CreateFile() using FILE_FLAG_NO_BUFFERING.
/// Similar to O_DIRECT: bypasses the Windows cache manager.
/// Requires buffer alignment to the volume sector size (typically 512 bytes).
/// We use the same 4096-aligned buffer as Linux.
///
/// ## Fallback
/// If direct I/O fails (unsupported filesystem, insufficient permissions,
/// very small files), we fall back to standard buffered I/O automatically.
/// The hash result is still correct — only the cache-bypass guarantee is lost.
fn hash_direct(path: &Path, gen_md5: bool, gen_xxh: bool) -> io::Result<FileHashes> {
    // Try platform-specific direct I/O first, fall back to buffered on error.
    let result = hash_direct_impl(path, gen_md5, gen_xxh);
    match result {
        Ok(h)  => Ok(h),
        Err(_) => {
            // Fallback: standard buffered read with pipeline.
            // Still correct (sync_all guarantees cache == disk),
            // just not direct from physical storage.
            hash_buffered(path, gen_md5, gen_xxh)
        }
    }
}

/// Platform-specific direct I/O implementation.
fn hash_direct_impl(path: &Path, gen_md5: bool, gen_xxh: bool) -> io::Result<FileHashes> {
    // Open with cache-bypassing flags — the file is moved into hash_buffered_file()
    // which passes it to the reader thread. No buffer needed here: the pipeline
    // allocates its own per-chunk buffers inside the reader thread.
    let file = open_direct(path)?;
    hash_buffered_file(file, gen_md5, gen_xxh)
}

// ── Platform-specific file openers ───────────────────────────────────────────

/// Opens a file with cache-bypassing flags.
/// Returns a standard fs::File (which wraps the OS file handle).
#[cfg(target_os = "linux")]
fn open_direct(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    // O_DIRECT (0x4000 on x86_64 Linux): bypass the page cache.
    // The kernel transfers data directly between the block device and
    // our userspace buffer — no copy through the page cache RAM.
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECT)
        .open(path)
}

#[cfg(target_os = "macos")]
fn open_direct(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::io::AsRawFd;
    // macOS: open normally, then disable the unified buffer cache (UBC)
    // via fcntl(F_NOCACHE). This must be done after opening.
    // F_NOCACHE = 48 on macOS — not exposed in std, use the raw constant.
    let file = fs::File::open(path)?;
    let ret = unsafe { libc::fcntl(file.as_raw_fd(), 48, 1) };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

#[cfg(target_os = "windows")]
fn open_direct(path: &Path) -> io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_FLAG_NO_BUFFERING (0x20000000): bypasses the Windows cache manager.
    // Combined with FILE_FLAG_SEQUENTIAL_SCAN (0x08000000) for sequential access hint.
    const FILE_FLAG_NO_BUFFERING:    u32 = 0x20000000;
    const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x08000000;
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN)
        .open(path)
}

// Fallback for any other OS (BSDs, etc.) — standard buffered open
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn open_direct(path: &Path) -> io::Result<fs::File> {
    fs::File::open(path)
}

// ── Note on buffer alignment ─────────────────────────────────────────────────
// The reader thread allocates per-chunk Vec<u8> via vec![0u8; HASH_CHUNK].
// On Linux, O_DIRECT requires 512-byte aligned buffers. The global allocator
// typically provides 8–16 byte alignment — marginal. If alignment errors occur
// the fallback to buffered I/O handles them. Future: use std::alloc with Layout.
// ── Core hash functions — pipelined read/hash ────────────────────────────────
//
// ## Why pipeline?
//
// The naive approach (read chunk → hash chunk → read next chunk) is sequential:
// the CPU sits idle while the disk reads, and the disk sits idle while the CPU
// hashes. This wastes half the available time.
//
// GTKHash solves this with an asynchronous state machine:
//   g_input_stream_read_async() → callback → g_thread_pool_push() → hash
// The next chunk is being read from disk while the previous chunk is being hashed.
//
// We implement the same principle using two OS threads and an mpsc channel:
//
//   Reader thread                    Hasher thread (caller)
//   ─────────────────────            ──────────────────────────
//   read chunk 1 from disk  ──────►  hash chunk 1
//   read chunk 2 from disk  ──────►  hash chunk 2
//   read chunk 3 from disk  ──────►  hash chunk 3
//   EOF → drop sender               finalize → return FileHashes
//
// The channel provides backpressure: if hashing is slower than reading,
// the reader blocks until the hasher consumes the previous chunk.
// If reading is slower than hashing (typical for NVMe), the hasher
// immediately processes each chunk as it arrives.
//
// ## Memory usage
//
// Two buffers of HASH_CHUNK bytes are in flight simultaneously:
//   - The reader thread fills buffer N+1
//   - The hasher thread processes buffer N
// Total extra RAM: 2 × HASH_CHUNK = 2 × 4 MiB = 8 MiB per file being hashed.
// Since multiple files are hashed in parallel (rayon), this multiplies by
// the number of concurrent hash operations. Acceptable for DIT workstations.
//
// ## Error handling
//
// If the reader thread encounters an I/O error, it sends Err(e) through the
// channel. The hasher thread receives it and propagates it as the function
// return value. The reader thread terminates cleanly (drop closes the sender,
// which causes the hasher's recv() to return Err, exiting its loop).

/// Hashes a file using a read/hash pipeline with native platform MD5.
///
/// ## Architecture — pipelined read/hash
///
/// A reader thread reads chunks via direct I/O (O_DIRECT/F_NOCACHE/NO_BUFFERING)
/// and sends them through an mpsc channel. The calling thread hashes each chunk
/// as it arrives — I/O and CPU overlap, matching GTKHash's async architecture.
///
/// ## MD5 implementation — native platform APIs
///
/// Instead of the pure-Rust `md-5` crate (~1 GB/s), we use the system crypto
/// library which uses hand-optimised assembly (SHA-NI, AVX2, NEON):
///
/// - Linux   : OpenSSL EVP (libcrypto) — 3–5 GB/s on modern x86_64
/// - macOS   : CommonCrypto CC_MD5     — 3–4 GB/s, Apple native
/// - Windows : CNG BCryptHashData      — 2–4 GB/s, Windows native
/// - Other   : md-5 RustCrypto fallback
///
/// ## XXH3
/// No native platform API supports XXH3 — we keep `xxhash-rust` which is
/// already heavily SIMD-optimised and reaches ~10 GB/s on modern CPUs.
fn hash_buffered_file(
    file:    fs::File,
    gen_md5: bool,
    gen_xxh: bool,
) -> io::Result<FileHashes> {
    // twox-hash 2.x: xxhash3_128::Hasher has its own write() and finish_128() methods
    use twox_hash::xxhash3_128::Hasher as Xxh3Hasher;
    use std::sync::mpsc;

    // Pipeline channel — bounded to 2 slots (one being hashed, one being read)
    let (tx, rx) = mpsc::sync_channel::<io::Result<Vec<u8>>>(2);

    // Reader thread — reads chunks and sends them through the channel
    std::thread::spawn(move || {
        let mut file = file;
        loop {
            // ── Aligned buffer allocation via aligned-vec ────────────────────
            //
            // O_DIRECT requires buffers aligned to the disk sector size.
            // Standard Vec<u8> aligns to 8–16 bytes — insufficient.
            //
            // aligned_vec::AVec<u8> guarantees configurable alignment.
            // ALIGN = 4096 bytes (one memory page) satisfies all sector sizes:
            //   512-byte sectors  (legacy HDD, some SSD)  ✓
            //   4096-byte sectors (modern NVMe, AF drives) ✓
            //
            // AVec<u8> is passed directly to io::Read::read() via DerefMut<[u8]>
            // — zero-copy: data goes straight from kernel to this aligned buffer,
            // then straight to the hash function. No intermediate copies.
            //
            // The buffer is allocated fresh each chunk. For the double-buffer
            // optimisation (allocate once, reuse), see the future roadmap.
            // ── 4096-byte aligned buffer via posix_memalign ─────────────────
            // O_DIRECT requires buffers aligned to the disk sector size.
            // posix_memalign() guarantees 4096-byte alignment on Linux/macOS.
            // On Windows we use _aligned_malloc(). On other platforms we use
            // a standard Vec<u8> (O_DIRECT is Linux/macOS/Windows specific).
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let (ptr, buf_len) = {
                let mut ptr: *mut libc::c_void = std::ptr::null_mut();
                let ret = unsafe {
                    libc::posix_memalign(&mut ptr, 4096, HASH_CHUNK)
                };
                if ret != 0 || ptr.is_null() {
                    tx.send(Err(io::Error::from_raw_os_error(ret))).ok();
                    break;
                }
                (ptr, HASH_CHUNK)
            };

            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            let (ptr, buf_len) = {
                let layout = std::alloc::Layout::from_size_align(HASH_CHUNK, 4096).unwrap();
                let raw = unsafe { std::alloc::alloc_zeroed(layout) };
                if raw.is_null() {
                    tx.send(Err(io::Error::new(io::ErrorKind::OutOfMemory, "alloc failed"))).ok();
                    break;
                }
                (raw, HASH_CHUNK)
            };

            // Wrap the raw pointer in a slice for reading.
            // SAFETY: ptr is valid, aligned, and exclusively owned by this thread.
            let buf_slice = unsafe {
                std::slice::from_raw_parts_mut(ptr as *mut u8, buf_len)
            };

            let read_result = io::Read::read(&mut file, buf_slice);

            match read_result {
                Ok(0) => {
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    unsafe { libc::free(ptr); }
                    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                    unsafe { std::alloc::dealloc(ptr, std::alloc::Layout::from_size_align(HASH_CHUNK, 4096).unwrap()); }
                    break; // EOF
                }
                Ok(n) => {
                    let chunk = buf_slice[..n].to_vec();
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    unsafe { libc::free(ptr); }
                    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                    unsafe { std::alloc::dealloc(ptr, std::alloc::Layout::from_size_align(HASH_CHUNK, 4096).unwrap()); }
                    if tx.send(Ok(chunk)).is_err() { break; }
                }
                Err(e) => {
                    #[cfg(any(target_os = "linux", target_os = "macos"))]
                    unsafe { libc::free(ptr); }
                    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
                    unsafe { std::alloc::dealloc(ptr, std::alloc::Layout::from_size_align(HASH_CHUNK, 4096).unwrap()); }
                    tx.send(Err(e)).ok();
                    break;
                }
            }
        }
    });

    // ── Hasher loop — native MD5 + xxhash-rust XXH3 ──────────────────────────

    // Initialise platform-specific MD5 context
    let mut md5_state = if gen_md5 { Some(NativeMd5::new()?) } else { None };
    let mut xxh_ctx   = Xxh3Hasher::with_seed(0);

    loop {
        match rx.recv() {
            Ok(Ok(chunk)) => {
                if let Some(ref mut ctx) = md5_state {
                    ctx.update(&chunk)?;
                }
                if gen_xxh { xxh_ctx.write(&chunk); }
            }
            Ok(Err(e)) => return Err(e),
            Err(_)     => break, // channel closed = EOF
        }
    }

    let md5 = if let Some(ctx) = md5_state {
        Some(ctx.finish()?)
    } else {
        None
    };

    Ok(FileHashes {
        md5,
        xxh: if gen_xxh { Some(format!("{:032x}", xxh_ctx.finish_128())) } else { None },
    })
}

/// Hashes a file using standard buffered I/O (fallback when direct I/O fails).
fn hash_buffered(
    path:    &Path,
    gen_md5: bool,
    gen_xxh: bool,
) -> io::Result<FileHashes> {
    let file = fs::File::open(path)?;
    hash_buffered_file(file, gen_md5, gen_xxh)
}

// ── Native MD5 implementations ────────────────────────────────────────────────
//
// Each platform has its own struct that wraps the native crypto context.
// All implement the same three methods: new(), update(&[u8]), finish() -> String.
// This uniform interface allows hash_buffered_file() to be platform-agnostic.

/// Linux — OpenSSL EVP API
///
/// OpenSSL's EVP (Envelope) API is the high-level interface to libcrypto.
/// It automatically selects the fastest available implementation at runtime:
///   - SHA-NI instructions (Intel/AMD since 2013)  → ~5 GB/s
///   - AVX2 vectorised implementation               → ~3 GB/s
///   - SSE2 fallback                                → ~1.5 GB/s
///
/// `libcrypto.so` is always present on Linux (it's a dependency of OpenSSH,
/// curl, git, and virtually every networked application). No installation needed.
/// `libssl-dev` (the headers) is required only at compile time, not at runtime.
///
/// EVP_MD_CTX_new() allocates the context on the heap (opaque C struct).
/// EVP_DigestInit_ex() initialises it with the MD5 algorithm.
/// EVP_DigestUpdate() feeds data chunks.
/// EVP_DigestFinal_ex() produces the 16-byte digest.
/// EVP_MD_CTX_free() releases the heap allocation.
#[cfg(target_os = "linux")]
struct NativeMd5 {
    ctx: openssl::hash::Hasher,
}

#[cfg(target_os = "linux")]
impl NativeMd5 {
    fn new() -> io::Result<Self> {
        openssl::hash::Hasher::new(openssl::hash::MessageDigest::md5())
            .map(|ctx| NativeMd5 { ctx })
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }
    fn update(&mut self, data: &[u8]) -> io::Result<()> {
        self.ctx.update(data)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }
    fn finish(mut self) -> io::Result<String> {
        self.ctx.finish()
            .map(|d| d.iter().map(|b| format!("{:02x}", b)).collect())
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e.to_string()))
    }
}

/// macOS — CommonCrypto CC_MD5
///
/// CommonCrypto is Apple's native cryptographic library, available on all
/// macOS versions. It is part of the Security framework and does not require
/// any installation or Cargo dependency — we call it directly via FFI.
///
/// CC_MD5_Init / CC_MD5_Update / CC_MD5_Final are the standard C functions.
/// They are declared in <CommonCrypto/CommonDigest.h> which we replicate here
/// via `extern "C"` declarations. The linker finds them in libSystem.dylib
/// (always linked on macOS) via the `-lSystem` flag from the Rust toolchain.
///
/// CC_MD5_CTX_SIZE: the size of the CC_MD5_CTX struct (92 bytes on all macOS).
/// We allocate it as a Vec<u8> to avoid a fixed-size array on the stack.
/// `unsafe` is required because we call C functions that take raw pointers.
#[cfg(target_os = "macos")]
struct NativeMd5 {
    /// Opaque CC_MD5_CTX context, allocated as bytes.
    ctx: Vec<u8>,
}

#[cfg(target_os = "macos")]
impl NativeMd5 {
    // Size of CC_MD5_CTX in bytes (defined in CommonCrypto/CommonDigest.h).
    // This is stable across all macOS versions and architectures.
    const CTX_SIZE: usize = 92;
    const DIGEST_LEN: usize = 16; // MD5 produces 128 bits = 16 bytes

    fn new() -> io::Result<Self> {
        extern "C" {
            // int CC_MD5_Init(CC_MD5_CTX *c);
            fn CC_MD5_Init(c: *mut u8) -> i32;
        }
        let mut ctx = vec![0u8; Self::CTX_SIZE];
        let ret = unsafe { CC_MD5_Init(ctx.as_mut_ptr()) };
        if ret != 1 {
            return Err(io::Error::new(io::ErrorKind::Other, "CC_MD5_Init failed"));
        }
        Ok(NativeMd5 { ctx })
    }

    fn update(&mut self, data: &[u8]) -> io::Result<()> {
        extern "C" {
            // int CC_MD5_Update(CC_MD5_CTX *c, const void *data, CC_LONG len);
            fn CC_MD5_Update(c: *mut u8, data: *const u8, len: u32) -> i32;
        }
        let ret = unsafe {
            CC_MD5_Update(self.ctx.as_mut_ptr(), data.as_ptr(), data.len() as u32)
        };
        if ret != 1 {
            Err(io::Error::new(io::ErrorKind::Other, "CC_MD5_Update failed"))
        } else {
            Ok(())
        }
    }

    fn finish(mut self) -> io::Result<String> {
        extern "C" {
            // unsigned char *CC_MD5_Final(unsigned char *md, CC_MD5_CTX *c);
            fn CC_MD5_Final(md: *mut u8, c: *mut u8) -> *mut u8;
        }
        let mut digest = vec![0u8; Self::DIGEST_LEN];
        unsafe { CC_MD5_Final(digest.as_mut_ptr(), self.ctx.as_mut_ptr()) };
        Ok(digest.iter().map(|b| format!("{:02x}", b)).collect())
    }
}

/// Windows — CNG (Cryptography Next Generation) BCrypt API
///
/// CNG is the modern Windows cryptographic API, available since Windows Vista.
/// It is always present — no installation required. We use `windows-sys` which
/// provides safe Rust bindings to the Win32 API without requiring OpenSSL.
///
/// BCryptOpenAlgorithmProvider → opens a handle to the MD5 algorithm provider
/// BCryptCreateHash           → creates a hash object
/// BCryptHashData             → feeds data to the hash
/// BCryptFinishHash           → produces the final digest
/// BCryptDestroyHash          → releases the hash object
/// BCryptCloseAlgorithmProvider → releases the algorithm handle
///
/// All these functions are in bcrypt.dll which is always loaded on Windows.
/// `windows-sys` generates the correct calling convention and type bindings.
#[cfg(target_os = "windows")]
struct NativeMd5 {
    alg:  windows_sys::Win32::Security::Cryptography::BCRYPT_ALG_HANDLE,
    hash: windows_sys::Win32::Security::Cryptography::BCRYPT_HASH_HANDLE,
}

#[cfg(target_os = "windows")]
impl NativeMd5 {
    fn new() -> io::Result<Self> {
        use windows_sys::Win32::Security::Cryptography::*;
        let mut alg  = 0usize as BCRYPT_ALG_HANDLE;
        let mut hash = 0usize as BCRYPT_HASH_HANDLE;
        unsafe {
            // BCRYPT_MD5_ALGORITHM = "MD5" (wide string)
            let status = BCryptOpenAlgorithmProvider(
                &mut alg,
                windows_sys::w!("MD5"),
                std::ptr::null(),
                0,
            );
            if status != 0 {
                return Err(io::Error::new(io::ErrorKind::Other,
                    format!("BCryptOpenAlgorithmProvider failed: {}", status)));
            }
            let status = BCryptCreateHash(alg, &mut hash,
                std::ptr::null_mut(), 0,
                std::ptr::null_mut(), 0, 0);
            if status != 0 {
                BCryptCloseAlgorithmProvider(alg, 0);
                return Err(io::Error::new(io::ErrorKind::Other,
                    format!("BCryptCreateHash failed: {}", status)));
            }
        }
        Ok(NativeMd5 { alg, hash })
    }

    fn update(&mut self, data: &[u8]) -> io::Result<()> {
        use windows_sys::Win32::Security::Cryptography::BCryptHashData;
        let status = unsafe {
            BCryptHashData(self.hash, data.as_ptr() as *mut u8, data.len() as u32, 0)
        };
        if status != 0 {
            Err(io::Error::new(io::ErrorKind::Other,
                format!("BCryptHashData failed: {}", status)))
        } else {
            Ok(())
        }
    }

    fn finish(self) -> io::Result<String> {
        use windows_sys::Win32::Security::Cryptography::*;
        let mut digest = vec![0u8; 16]; // MD5 = 128 bits = 16 bytes
        let status = unsafe {
            BCryptFinishHash(self.hash, digest.as_mut_ptr(), digest.len() as u32, 0)
        };
        unsafe {
            BCryptDestroyHash(self.hash);
            BCryptCloseAlgorithmProvider(self.alg, 0);
        }
        if status != 0 {
            return Err(io::Error::new(io::ErrorKind::Other,
                format!("BCryptFinishHash failed: {}", status)));
        }
        Ok(digest.iter().map(|b| format!("{:02x}", b)).collect())
    }
}

#[cfg(target_os = "windows")]
impl Drop for NativeMd5 {
    fn drop(&mut self) {
        // Safety: handles are valid if new() succeeded.
        // Drop is called even if finish() panics, so we clean up here too.
        use windows_sys::Win32::Security::Cryptography::*;
        unsafe {
            if self.hash != 0 as _ { BCryptDestroyHash(self.hash); }
            if self.alg  != 0 as _ { BCryptCloseAlgorithmProvider(self.alg, 0); }
        }
    }
}

/// Other platforms (BSDs, etc.) — RustCrypto md-5 fallback
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
struct NativeMd5 {
    ctx: md5::Md5,
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
impl NativeMd5 {
    fn new() -> io::Result<Self> {
        use md5::Digest;
        Ok(NativeMd5 { ctx: md5::Md5::new() })
    }
    fn update(&mut self, data: &[u8]) -> io::Result<()> {
        use md5::Digest;
        self.ctx.update(data);
        Ok(())
    }
    fn finish(self) -> io::Result<String> {
        use md5::Digest;
        Ok(format!("{:x}", self.ctx.finalize()))
    }
}

// ── Report generation ─────────────────────────────────────────────────────────

/// Generates all requested output files (.md5, .xxh3, .csv, .pdf) for each destination.
///
/// Extracted as a separate function to avoid duplicating the report-writing
/// logic between the "copy-only" path and the "copy + verify" path in `run()`.
///
/// ## Parameters
/// - `has_verify` : if `true`, entries carry a verification status (OK/ERROR).
///                  If `false` (copy-only mode), the Status column is omitted.
///
/// ## `#[allow(clippy::too_many_arguments)]`
/// Clippy (Rust's linter) warns when a function has more than 7 parameters.
/// We suppress this warning here because the parameters are all distinct and
/// necessary — grouping them into a struct would add complexity without clarity.
/// Alternative: pass a `ReportConfig` struct. Trade-off: more code, no real benefit.
#[allow(clippy::too_many_arguments)]
fn generate_reports(
    tx:           &Sender<Msg>,
    destinations: &[PathBuf],
    src_name:     &str,
    src:          &Path,
    meta_entries: &[(String, metadata::FileMeta)],
    hashes:       &[(FileHashes, bool)],
    gen_csv:      bool,
    gen_pdf:      bool,
    gen_html:     bool,
    gen_md5:      bool,
    gen_xxh:      bool,
    has_verify:   bool,
    settings:     &Settings,
) {
    let _ = tx.send(Msg::Progress(0.98, "Generating reports…".into()));
    for dst in destinations {
        if gen_md5 {
            let p = dst.join(format!("{}_checksum.md5", src_name));
            match write_checksum(&p, meta_entries, hashes, |h| h.md5.clone()) {
                Ok(_)  => log(tx, &format!("◈  MD5 : {}\n", p.display())),
                Err(e) => log(tx, &format!("✖  MD5 error: {}\n", e)),
            }
        }
        if gen_xxh {
            let p = dst.join(format!("{}_checksum.xxh3", src_name));
            match write_checksum(&p, meta_entries, hashes, |h| h.xxh.clone()) {
                Ok(_)  => log(tx, &format!("◈  XXH3: {}\n", p.display())),
                Err(e) => log(tx, &format!("✖  XXH3 error: {}\n", e)),
            }
        }
        if gen_csv {
            // Build CSV entries carrying both md5 and xxh3 hashes separately.
            // The tuple is (FileMeta, md5: String, xxh3: String, status: Option<bool>).
            // Empty string means that hash type was not computed.
            let csv_entries: Vec<(metadata::FileMeta, String, String, Option<bool>)> =
                meta_entries.iter().zip(hashes.iter())
                    .map(|((_, meta), (fh, ok))| {
                        let md5  = fh.md5.clone().unwrap_or_default();
                        let xxh3 = fh.xxh.clone().unwrap_or_default();
                        (meta.clone(), md5, xxh3, if has_verify { Some(*ok) } else { None })
                    })
                    .collect();
            match metadata::write_csv(dst, src_name, &csv_entries, settings, gen_md5, gen_xxh) {
                Ok(_)  => log(tx, &format!("◈  CSV : {}\n",
                    dst.join(format!("{}_report.csv", src_name)).display())),
                Err(e) => log(tx, &format!("✖  CSV error: {}\n", e)),
            }
        }
        if gen_pdf {
            // PDF entries: (FileMeta, md5: String, xxh3: String, rel_path: String, status).
            // Both md5 and xxh3 are passed separately so the PDF can show the right column.
            let pdf_entries: Vec<(metadata::FileMeta, String, String, String, Option<bool>)> =
                meta_entries.iter().zip(hashes.iter())
                    .map(|((rel, meta), (fh, ok))| {
                        let md5  = fh.md5.clone().unwrap_or_default();
                        let xxh3 = fh.xxh.clone().unwrap_or_default();
                        (meta.clone(), md5, xxh3, rel.clone(),
                         if has_verify { Some(*ok) } else { None })
                    })
                    .collect();
            log(tx, "◎  Generating PDF report…\n");
            match pdf_report::write_pdf(dst, src_name, src, &pdf_entries, settings, gen_md5, gen_xxh) {
                Ok(_)  => log(tx, &format!("◈  PDF : {}\n",
                    dst.join(format!("{}_report.pdf", src_name)).display())),
                Err(e) => log(tx, &format!("✖  PDF error: {}\n", e)),
            }
        }
        if gen_html {
            let html_entries: Vec<(metadata::FileMeta, String, String, String, Option<bool>)> =
                meta_entries.iter().zip(hashes.iter())
                    .map(|((rel, meta), (fh, ok))| {
                        let md5  = fh.md5.clone().unwrap_or_default();
                        let xxh3 = fh.xxh.clone().unwrap_or_default();
                        (meta.clone(), md5, xxh3, rel.clone(),
                         if has_verify { Some(*ok) } else { None })
                    })
                    .collect();
            log(tx, "◎  Generating HTML report…\n");
            match html_report::write_html(dst, src_name, src, &html_entries, settings, gen_md5, gen_xxh) {
                Ok(_)  => log(tx, &format!("◈  HTML: {}\n",
                    dst.join(format!("{}_report.html", src_name)).display())),
                Err(e) => log(tx, &format!("✖  HTML error: {}\n", e)),
            }
        }
    }
}

// ── Checksum file writer ──────────────────────────────────────────────────────

fn write_checksum<F>(
    path:    &Path,
    entries: &[(String, metadata::FileMeta)],
    hashes:  &[(FileHashes, bool)],
    hash_fn: F,
) -> io::Result<()>
where F: Fn(&FileHashes) -> Option<String>
{
    use std::io::Write;
    let mut f = fs::File::create(path)?;
    for ((rel, _), (fh, _)) in entries.iter().zip(hashes.iter()) {
        if let Some(hash) = hash_fn(fh) {
            writeln!(f, "{}  {}", hash, rel)?;
        }
    }
    Ok(())
}

// ── Filesystem helpers ────────────────────────────────────────────────────────

/// Recursively collects all **files** (not directories) under `dir`.
///
/// Returns a flat `Vec<PathBuf>` containing the absolute path of every file
/// found in `dir` and all its subdirectories, in filesystem traversal order.
///
/// Directories are traversed but not included in the result — only files.
/// Hidden files and dotfiles are included (no filtering by name).
///
/// ## Why split into two functions?
/// `collect_files` is the public entry point that initialises the accumulator.
/// `collect_recursive` is the private recursive helper that does the actual work.
/// This avoids exposing `out: &mut Vec<PathBuf>` in the public API.
fn collect_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_recursive(dir, &mut out)?;
    Ok(out)
}

/// Recursive implementation of `collect_files`.
///
/// `out: &mut Vec<PathBuf>` : mutable reference to the accumulator in the caller.
/// `&mut` : we borrow the Vec mutably (can push to it) but don't own it.
/// The `?` operator on `fs::read_dir(dir)?` : if reading fails (permission
/// denied, etc.), return `Err(e)` immediately — propagates up the call stack.
/// `entry?.path()` : `entry` is `io::Result<DirEntry>`, `?` unwraps or returns.
fn collect_recursive(dir: &Path, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() { collect_recursive(&path, out)?; } // recurse into subdirs
        else             { out.push(path); }                  // add files to accumulator
    }
    Ok(()) // explicit Ok(()) : this function produces no meaningful value on success
}

// ── Log and ETA ──────────────────────────────────────────────────────────────

/// Sends a log line to the UI via the `Msg::Log` channel message.
///
/// `let _ = …` : the `send()` return value (`Result<(), SendError>`) is
/// intentionally discarded. A send failure only happens if the receiver
/// (the Tauri forwarding thread) has been dropped — which means the window
/// is closing. In that case, silently dropping the log is correct behaviour.
/// Without `let _ = …`, the compiler would emit an "unused Result" warning.
fn log(tx: &Sender<Msg>, msg: &str) {
    let _ = tx.send(Msg::Log(msg.to_string()));
}

/// Returns bytes-per-second from a sliding 2-second window.
/// Returns 0.0 during the initial warmup period.
fn speed_bps(
    done: u64, snap_bytes: &mut u64, snap_time: &mut Instant, global: &Instant,
) -> f64 {
    let elapsed = global.elapsed().as_secs_f64();
    if elapsed < 0.5 || done == 0 { return 0.0; }
    let snap_elapsed = snap_time.elapsed().as_secs_f64();
    if snap_elapsed >= 2.0 && done > *snap_bytes {
        let delta = done - *snap_bytes;
        *snap_bytes = done;
        *snap_time = Instant::now();
        return delta as f64 / snap_elapsed;
    }
    if *snap_bytes == 0 {
        return done as f64 / elapsed;
    }
    done.saturating_sub(*snap_bytes) as f64 / snap_elapsed.max(0.1)
}

/// Formats a byte-per-second value as a human-readable speed string.
/// Returns "…" during the initial warmup before a measurement is available.
fn fmt_speed(bps: f64) -> String {
    if bps < 100.0 { return "…".to_string(); }
    if bps >= 1_073_741_824.0 {
        format!("{:.1} GB/s", bps / 1_073_741_824.0)
    } else if bps >= 1_048_576.0 {
        format!("{:.0} MB/s", bps / 1_048_576.0)
    } else {
        format!("{:.0} KB/s", bps / 1_024.0)
    }
}
