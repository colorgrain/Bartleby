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
//!   syncfs(dst1) … syncfs(dstN)  ← one call per filesystem, after all files
//!
//! Phase 2 ── Integrity verification ──────────────────────────────────────
//!   [Linux/FUSE: fadvise(DONTNEED) per FUSE path before each hash]
//!   For each file (sequentially):
//!     ┌── hash(src)  ──┐
//!     ├── hash(dst1) ──┤  ← rayon parallel, O_DIRECT or fadvise+buffered
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
//! After `syncfs()` (Linux) or `sync_all()` (other platforms), the kernel page
//! cache and physical disk are identical. However, reading through the cache
//! would hash RAM copies of the data rather than verifying the physical disk.
//! O_DIRECT forces reads directly from the storage device for a true end-to-end
//! integrity check.
//!
//! ## FUSE filesystems (Linux: ntfs-3g, fuse-exfat …)
//!
//! O_DIRECT fails with EINVAL on FUSE filesystems (ntfs-3g, fuse-exfat, sshfs).
//! Two mitigations are applied on Linux when a FUSE destination is detected:
//!
//! 1. **Phase 1**: `syncfs()` replaces `sync_all()` per file. One flush per
//!    filesystem covers all files at once. This avoids the ntfs-3g performance
//!    cliff where every `fsync()` triggers a full FUSE cache flush.
//!
//! 2. **Phase 2**: `posix_fadvise(FADV_DONTNEED)` is called on each FUSE-backed
//!    path before hashing. This evicts the pages from the OS page cache so the
//!    subsequent buffered read is forced through the FUSE driver to the physical
//!    disk — restoring the disk-to-disk guarantee despite the absence of O_DIRECT.
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
use std::sync::{Arc, Condvar, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
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

// ── Pause / Cancel control ────────────────────────────────────────────────────

/// Shared pause/cancel state threaded through the copy engine.
///
/// - `paused`    : a Condvar-guarded bool. When `true`, worker threads block
///   on `wait_if_paused()` between 64 MiB chunks until `resume()` is called.
/// - `cancelled` : an atomic flag. When set, `wait_if_paused()` returns `Err(())`
///   so the caller can clean up and return immediately.
pub struct PauseCancel {
    paused:    (Mutex<bool>, Condvar),
    cancelled: AtomicBool,
}

impl PauseCancel {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            paused:    (Mutex::new(false), Condvar::new()),
            cancelled: AtomicBool::new(false),
        })
    }
    pub fn pause(&self) { *self.paused.0.lock().unwrap() = true; }
    pub fn resume(&self) {
        *self.paused.0.lock().unwrap() = false;
        self.paused.1.notify_all();
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.resume();
    }
    pub fn is_cancelled(&self) -> bool { self.cancelled.load(Ordering::Acquire) }
    pub fn wait_if_paused(&self) -> Result<(), ()> {
        if self.is_cancelled() { return Err(()); }
        let (lock, cvar) = &self.paused;
        let guard = lock.lock().unwrap();
        if !*guard { return Ok(()); }
        let _guard = cvar.wait_while(guard, |p| *p && !self.is_cancelled()).unwrap();
        if self.is_cancelled() { Err(()) } else { Ok(()) }
    }
}

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

/// Information about one conflicting file sent to the UI for the conflict dialog.
///
/// `size_match` and `date_match` are `true` only if **all** conflicting destinations
/// have the same size / modification time as the source. A single mismatch → `false`.
pub struct ConflictInfo {
    pub rel_path:   String,
    pub size_match: bool,
    pub date_match: bool,
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
    Conflicts(Vec<ConflictInfo>),
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

/// Per-destination conflict data — used internally during the pre-copy conflict check.
struct ConflictEntry {
    rel_path:    String,
    dst_matches: Vec<(usize, bool, bool)>, // (dst_idx, size_match, date_match)
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
///                    `gen_md5 || gen_xxh || gen_size` — pre-computed by the caller.
/// - `gen_md5`      : compute MD5, verify destination, write `.md5` file.
/// - `gen_xxh`      : compute XXH3-128, verify destination, write `.xxh3` file.
/// - `gen_size`     : compare source vs destination file sizes (fast, no checksum file).
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
    src:               PathBuf,
    destinations:      Vec<PathBuf>,
    verify:            bool,
    gen_md5:           bool,
    gen_xxh:           bool,
    gen_size:          bool,
    gen_csv:           bool,
    gen_pdf:           bool,
    gen_html:          bool,
    copy_as_subfolder: bool,
    comment:           String,
    settings:          Settings,
    tx:                Sender<Msg>,
    reply_rx:          std::sync::mpsc::Receiver<Reply>,
    pc:                Arc<PauseCancel>,
) {
    let start = Instant::now();

    log(&tx, &format!("→  Source : {}\n", src.display()));
    for dst in &destinations { log(&tx, &format!("→  Dest   : {}\n", dst.display())); }
    if gen_md5  { log(&tx, "→  Hash   : MD5\n"); }
    if gen_xxh  { log(&tx, "→  Hash   : XXH3-128\n"); }
    if gen_size { log(&tx, "→  Verify : file size\n"); }
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
        .filter(|d| {
            let check = if copy_as_subfolder { d.join(&src_name) } else { d.to_path_buf() };
            fs::read_dir(&check).map(|mut r| r.next().is_some()).unwrap_or(false)
        })
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

    // Detect conflicts per (file, destination) pair, comparing size and modification time.
    let conflict_entries: Vec<ConflictEntry> = files.iter().filter_map(|p| {
        let rel = p.strip_prefix(&src).unwrap_or(p);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let src_meta = fs::metadata(p).ok();
        let dst_matches: Vec<(usize, bool, bool)> = destinations.iter().enumerate()
            .filter_map(|(i, d)| {
                let dst_path = if copy_as_subfolder { d.join(&src_name).join(rel) } else { d.join(rel) };
                if !dst_path.exists() { return None; }
                let dst_meta = fs::metadata(&dst_path).ok();
                let size_match = match (&src_meta, &dst_meta) {
                    (Some(sm), Some(dm)) => sm.len() == dm.len(),
                    _ => false,
                };
                let date_match = match (&src_meta, &dst_meta) {
                    (Some(sm), Some(dm)) => sm.modified().ok() == dm.modified().ok(),
                    _ => false,
                };
                Some((i, size_match, date_match))
            })
            .collect();
        if dst_matches.is_empty() { None } else { Some(ConflictEntry { rel_path: rel_str, dst_matches }) }
    }).collect();

    let conflict_infos: Vec<ConflictInfo> = conflict_entries.iter()
        .map(|e| ConflictInfo {
            rel_path:   e.rel_path.clone(),
            size_match: e.dst_matches.iter().all(|(_, sm, _)| *sm),
            date_match: e.dst_matches.iter().all(|(_, _, dm)| *dm),
        })
        .collect();

    // skip_pairs: (rel_path, dst_index) — only skip where size AND date both match.
    let mut skip_pairs: std::collections::HashSet<(String, usize)> =
        std::collections::HashSet::new();
    if !conflict_entries.is_empty() {
        let _ = tx.send(Msg::Conflicts(conflict_infos));
        match reply_rx.recv().unwrap_or(Reply::Cancel) {
            Reply::Cancel => {
                log(&tx, "✖  Cancelled.\n");
                let _ = tx.send(Msg::Done(false, "Cancelled.".into()));
                return;
            }
            Reply::Skip => {
                for entry in &conflict_entries {
                    for &(dst_idx, size_match, date_match) in &entry.dst_matches {
                        if size_match && date_match {
                            skip_pairs.insert((entry.rel_path.clone(), dst_idx));
                        }
                    }
                }
                log(&tx, &format!("△  Skipping {} file×destination pair(s) where size & date match.\n", skip_pairs.len()));
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
    // gen_size Phase 2 is pure stat() calls — no bytes read, no progress weight.
    let grand_total = total_bytes * n_dst
        + if gen_md5 || gen_xxh { total_bytes * (1 + n_dst) } else { 0 };
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
    // Linux sync strategy: instead of sync_all() (fsync) after each individual
    // file, a single syncfs() per destination filesystem is issued at the END
    // of the whole copy loop. syncfs(2) flushes all dirty pages on the filesystem
    // in one kernel round-trip, equivalent to fsync() on every written file but
    // with far fewer IPC transitions to FUSE daemons (ntfs-3g, fuse-exfat) where
    // each fsync triggers a full flush and is very expensive.
    //
    // Other platforms: sync_all() (fsync) is called per-file as before, giving
    // per-file crash-safety granularity.
    // ══════════════════════════════════════════════════════════════════════════

    // ── Linux: detect FUSE-mounted filesystems ────────────────────────────────
    //
    // FUSE filesystems (ntfs-3g, fuse-exfat, sshfs …) have two critical quirks:
    //
    //   1. O_DIRECT returns EINVAL: Phase 2 hash_direct() falls back to buffered
    //      I/O, which reads from the page cache instead of the physical disk.
    //      Without mitigation, this breaks the disk-to-disk integrity guarantee.
    //
    //   2. fsync is slow: ntfs-3g serialises every fsync through the FUSE daemon,
    //      making N×M per-file fsync calls very expensive (N files × M destinations).
    //
    // Mitigations applied when FUSE is detected:
    //   • Phase 1 end   : syncfs() once per destination filesystem (see below).
    //   • Phase 2 start : posix_fadvise(FADV_DONTNEED) on each FUSE path before
    //                     hashing, evicting pages so the buffered read hits disk.
    //
    // FUSE_SUPER_MAGIC = 0x65735546 (linux/magic.h) — reported by all FUSE mounts.
    #[cfg(target_os = "linux")]
    let (fuse_src, fuse_dests, any_fuse) = {
        let src_fuse   = is_fuse_path(&src);
        let dests_fuse: Vec<bool> = destinations.iter().map(|d| is_fuse_path(d)).collect();
        let any        = src_fuse || dests_fuse.iter().any(|&f| f);
        (src_fuse, dests_fuse, any)
    };

    #[cfg(target_os = "linux")]
    if any_fuse {
        log(&tx, "△  FUSE filesystem detected (e.g. ntfs-3g)\n");
        log(&tx, "   Phase 1: syncfs per filesystem instead of per-file fsync\n");
        log(&tx, "   Phase 2: fadvise(DONTNEED) before each hash to ensure disk reads\n\n");
    }

    log(&tx, &format!("── Phase 1 — Copy {} ──────────────────────\n", ts(&start)));

    let mut copied: Vec<(PathBuf, String)> = Vec::new();
    let mut copy_errors = 0usize;

    for (idx, src_path) in files.iter().enumerate() {
        if let Err(_) = pc.wait_if_paused() {
            log(&tx, "✖  Cancelled.\n");
            let _ = tx.send(Msg::Done(false, "Cancelled.".into()));
            return;
        }
        let rel     = src_path.strip_prefix(&src).unwrap_or(src_path);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let fsize   = file_sizes[idx];

        let pct = if grand_total > 0 {
            (bytes_done as f64 / grand_total as f64).min(0.98)
        } else { 0.0 };
        let rs = fmt_speed(speed_bps(p1_src_bytes, &mut p1_src_snap, &mut p1_src_t, &start));
        let ws = fmt_speed(speed_bps(p1_dst_bytes, &mut p1_dst_snap, &mut p1_dst_t, &start));
        let _ = tx.send(Msg::Progress(pct, format!("Copying {} — R: {}  W: {}", rel_str, rs, ws)));

        // Filter destinations: exclude those where this file is already complete (size + date match).
        let dst_entries: Vec<(usize, PathBuf)> = destinations.iter().enumerate()
            .filter(|(i, _)| !skip_pairs.contains(&(rel_str.clone(), *i)))
            .map(|(i, d)| (i, if copy_as_subfolder { d.join(&src_name).join(rel) } else { d.join(rel) }))
            .collect();

        for (i, d) in destinations.iter().enumerate() {
            if skip_pairs.contains(&(rel_str.clone(), i)) {
                log(&tx, &format!("  ↷  skipped (size & date match): {} → {}\n", rel_str, d.display()));
            }
        }

        if dst_entries.is_empty() {
            // All destinations already have this file — add to `copied` for verification/reports.
            copied.push((src_path.clone(), rel_str.clone()));
            bytes_done += fsize * n_dst;
            continue;
        }

        let dst_paths: Vec<PathBuf> = dst_entries.iter().map(|(_, p)| p.clone()).collect();
        let dst_indices: Vec<usize> = dst_entries.iter().map(|(i, _)| *i).collect();

        // Create subdirectories in active destinations only.
        let mut dir_ok = true;
        for dst_path in &dst_paths {
            if let Some(parent) = dst_path.parent() {
                if fs::create_dir_all(parent).is_err() { dir_ok = false; }
            }
        }
        if !dir_ok { copy_errors += 1; continue; }

        // Copy to active destinations in parallel.
        let copy_results: Vec<io::Result<u64>> = dst_paths.par_iter()
            .map(|dst_path| copy_file(src_path, dst_path, &pc))
            .collect();

        let mut any_error = false;
        for (i, res) in copy_results.iter().enumerate() {
            if let Err(e) = res {
                if e.kind() == io::ErrorKind::Interrupted {
                    log(&tx, "✖  Cancelled.\n");
                    let _ = tx.send(Msg::Done(false, "Cancelled.".into()));
                    return;
                }
                log(&tx, &format!("  ✖  → {}: {}\n", destinations[dst_indices[i]].display(), e));
                any_error = true;
            }
        }
        if any_error { copy_errors += 1; continue; }

        // On non-Linux: fsync each destination file immediately after copy.
        // Per-file fsync gives crash-safety granularity: if the process is
        // killed mid-transfer, every file copied so far is safely on disk.
        //
        // On Linux: per-file fsync is intentionally omitted here. A single
        // syncfs() call after *all* files are copied (see below) replaces
        // N×M fsync calls. This is far more efficient on FUSE filesystems
        // where every fsync triggers a round-trip through the FUSE daemon.
        // syncfs(2) provides the same durability guarantee: all dirty pages
        // on the filesystem are flushed to physical storage in one pass.
        #[cfg(not(target_os = "linux"))]
        {
            let sync_results: Vec<io::Result<()>> = dst_paths.par_iter()
                .map(|dst_path| fs::OpenOptions::new().write(true).open(dst_path)?.sync_all())
                .collect();
            for (i, res) in sync_results.iter().enumerate() {
                if let Err(e) = res {
                    log(&tx, &format!("  ✖  sync {}: {}\n",
                        destinations[dst_indices[i]].display(), e));
                    copy_errors += 1;
                }
            }
        }

        bytes_done   += fsize * n_dst;
        p1_src_bytes += fsize;
        p1_dst_bytes += fsize * (dst_entries.len() as u64);
        copied.push((src_path.clone(), rel_str.clone()));
        log(&tx, &format!("  ✓  {}\n", rel_str));
    }

    // ── Linux: single syncfs() per destination filesystem ────────────────────
    //
    // All file copies are now buffered in the kernel page cache (data is correct
    // in RAM, not yet guaranteed on physical disk). We flush everything at once
    // with syncfs(2), which is equivalent to calling fsync() on every written
    // file on that filesystem, but in one kernel round-trip instead of N×M.
    //
    // We deduplicate by st_dev (device number): two destination paths that share
    // the same mounted filesystem (same st_dev) need only one syncfs() call.
    // In the common case — each destination on a separate drive — there is one
    // syncfs() per destination, matching the previous per-file fsync() count.
    //
    // syncfs() is called unconditionally, even when verify=false: the user always
    // wants their data persisted on disk, regardless of hash verification.
    //
    // If syncfs() fails, we log a warning and continue. Phase 2 (hash comparison)
    // will detect any data corruption caused by the incomplete flush. The error
    // is not counted as a copy_error because the write() calls all succeeded —
    // the data is correct in the kernel buffer; only the flush to disk is uncertain.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        // Track which physical devices have already been synced.
        // st_dev uniquely identifies a mounted filesystem on Linux.
        let mut synced_devs: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for dst in &destinations {
            // stat() the destination root to obtain its device number.
            // On error (destination was never created), skip silently.
            let dev = fs::metadata(dst).map(|m| m.dev()).unwrap_or(0);
            if synced_devs.insert(dev) {
                // This device has not been synced yet — flush it now.
                if let Err(e) = syncfs_dir(dst) {
                    log(&tx, &format!(
                        "  △  syncfs {} failed: {} (Phase 2 will verify integrity)\n",
                        dst.display(), e));
                }
            }
            // dev == 0 or already synced: skip without logging (normal case).
        }
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
        generate_reports(&tx, &destinations, &src_name, &src, total_bytes,
                         &meta_entries, &no_hashes,
                         gen_csv, gen_pdf, gen_html, gen_md5, gen_xxh, false, &comment, &settings);
        let summary = format!("✓  {} file(s) copied to {} destination(s) — no verification",
            copied.len(), destinations.len());
        log(&tx, &format!("\n{}\n", summary));
        let _ = tx.send(Msg::Progress(1.0, "Done".into()));
        let _ = tx.send(Msg::Done(true, summary));
        return;
    }

    // ══════════════════════════════════════════════════════════════════════════
    // PHASE 2 — Verification
    //
    // Two paths depending on what the user requested:
    //
    // A) Size check (gen_size): compare source vs destination file sizes via
    //    fs::metadata() — no disk reads, completes in milliseconds.
    //
    // B) Hash check (gen_md5 / gen_xxh): re-read source and all destinations
    //    with O_DIRECT / F_NOCACHE / FILE_FLAG_NO_BUFFERING to hash what is
    //    physically on disk, then compare digests.
    // ══════════════════════════════════════════════════════════════════════════
    log(&tx, &format!("\n── Phase 2 — Verification {} ──────────────\n", ts(&start)));

    let n_files = copied.len();
    let mut results: Vec<(FileHashes, bool)> =
        vec![(FileHashes::default(), true); n_files];
    let mut verify_errors = 0usize;

    if gen_size {
        // ── Path A: fast file-size comparison ────────────────────────────────
        for (file_idx, (_src_path, rel_str)) in copied.iter().enumerate() {
            if let Err(_) = pc.wait_if_paused() {
                log(&tx, "✖  Cancelled.\n");
                let _ = tx.send(Msg::Done(false, "Cancelled.".into()));
                return;
            }
            let src_size = file_sizes[file_idx];

            let pct = if grand_total > 0 {
                (bytes_done as f64 / grand_total as f64).min(0.98)
            } else { 0.98 };
            let _ = tx.send(Msg::Progress(pct,
                format!("Checking size {}/{} — {}", file_idx + 1, n_files, rel_str)));

            let mut mismatch = false;
            for (i, dst) in destinations.iter().enumerate() {
                let rel_native = rel_str.replace('/', std::path::MAIN_SEPARATOR_STR);
                let dst_path = if copy_as_subfolder {
                    dst.join(&src_name).join(&rel_native)
                } else {
                    dst.join(&rel_native)
                };
                match fs::metadata(&dst_path) {
                    Ok(m) if m.len() != src_size => {
                        log(&tx, &format!(
                            "  ✖  SIZE MISMATCH — {}\n     src: {} B  dst[{}]: {} B\n",
                            rel_str, src_size, i + 1, m.len()));
                        mismatch = true;
                    }
                    Err(e) => {
                        log(&tx, &format!(
                            "  ✖  stat error {} (dst[{}]): {}\n", rel_str, i + 1, e));
                        mismatch = true;
                    }
                    _ => {}
                }
            }
            if mismatch {
                verify_errors += 1;
                results[file_idx].1 = false;
            } else {
                log(&tx, &format!("  ✓  {}  [{} B]\n", rel_str, src_size));
            }
        }
    } else {
        // ── Path B: hash from physical storage ───────────────────────────────
        //
        // Reads use O_DIRECT on Linux, F_NOCACHE on macOS, FILE_FLAG_NO_BUFFERING
        // on Windows. This ensures we hash what is physically on disk, not cached RAM.
        //
        // For each file: source + all destinations are hashed in parallel.
        // MD5 and XXH3 are computed simultaneously in a single read pass.
        for (file_idx, (src_path, rel_str)) in copied.iter().enumerate() {
            if let Err(_) = pc.wait_if_paused() {
                log(&tx, "✖  Cancelled.\n");
                let _ = tx.send(Msg::Done(false, "Cancelled.".into()));
                return;
            }
            let fsize = fs::metadata(src_path).map(|m| m.len()).unwrap_or(0);

            let pct = if grand_total > 0 {
                (bytes_done as f64 / grand_total as f64).min(0.98)
            } else { 0.0 };
            let vs = fmt_speed(speed_bps(p2_bytes, &mut p2_snap, &mut p2_t, &start));
            let _ = tx.send(Msg::Progress(pct,
                format!("Verifying {}/{} — {} — R: {}", file_idx + 1, n_files, rel_str, vs)));

            // Build the list of paths to hash: [source, destination1, destination2, …]
            let mut all_paths: Vec<PathBuf> = vec![src_path.clone()];
            for dst in &destinations {
                let rel_native = rel_str.replace('/', std::path::MAIN_SEPARATOR_STR);
                let path = if copy_as_subfolder {
                    dst.join(&src_name).join(&rel_native)
                } else {
                    dst.join(&rel_native)
                };
                all_paths.push(path);
            }

            // ── Linux: evict FUSE-backed pages from the OS cache before hashing ─
            //
            // For non-FUSE paths (ext4, btrfs, XFS …), `hash_direct()` uses O_DIRECT
            // which bypasses the page cache entirely — eviction is unnecessary.
            //
            // For FUSE paths (ntfs-3g, fuse-exfat …), O_DIRECT returns EINVAL, so
            // `hash_direct()` falls back to standard buffered I/O. Without eviction,
            // that buffered read would hash the in-RAM page cache copy written during
            // Phase 1, rather than verifying what is physically on disk.
            //
            // `posix_fadvise(FADV_DONTNEED)` asks the kernel to release the cached
            // pages for this file. After Phase 1's syncfs(), all pages are clean
            // (written to disk), so the kernel can evict them immediately. The next
            // read call goes to the FUSE driver → physical disk, restoring the
            // disk-to-disk integrity guarantee.
            //
            // all_paths layout: [0] = source, [1..] = destinations (same order as
            // `destinations`), so fuse_dests[i-1] maps to all_paths[i] for i >= 1.
            #[cfg(target_os = "linux")]
            {
                for (i, path) in all_paths.iter().enumerate() {
                    let path_is_fuse = if i == 0 { fuse_src } else { fuse_dests[i - 1] };
                    if path_is_fuse {
                        fadvise_dontneed(path);
                    }
                }
            }

            let hash_results: Vec<io::Result<FileHashes>> = all_paths.par_iter()
                .map(|path| hash_direct(path, gen_md5, gen_xxh, &pc))
                .collect();

            let mut read_error = false;
            for (i, res) in hash_results.iter().enumerate() {
                if let Err(e) = res {
                    if e.kind() == io::ErrorKind::Interrupted {
                        log(&tx, "✖  Cancelled.\n");
                        let _ = tx.send(Msg::Done(false, "Cancelled.".into()));
                        return;
                    }
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
    }

    log(&tx, &format!("\n── Phase 2 complete {} ─────────────────────\n", ts(&start)));

    // ── Phase 3: reports ──────────────────────────────────────────────────────
    generate_reports(&tx, &destinations, &src_name, &src, total_bytes,
                     &meta_entries, &results,
                     gen_csv, gen_pdf, gen_html, gen_md5, gen_xxh, true, &comment, &settings);

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
    let hash_label = match (gen_md5, gen_xxh, gen_size) {
        (true,  true,  _) => " — MD5 + XXH3 verified",
        (true,  false, _) => " — MD5 verified",
        (false, true,  _) => " — XXH3 verified",
        (false, false, true) => " — size verified",
        _                    => "",
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
/// After `syncfs()` (Linux) or `sync_all()` (other platforms), the page cache
/// and physical disk are identical. However, reading through the cache means we
/// might be comparing two identical RAM copies rather than verifying what is on
/// disk. Direct I/O forces the read to come from the storage device itself,
/// making the verification a true end-to-end integrity check.
///
/// ## FUSE fallback
///
/// On FUSE filesystems (ntfs-3g, fuse-exfat …), O_DIRECT returns EINVAL.
/// The caller (`run()`) must call `posix_fadvise(FADV_DONTNEED)` on the path
/// before invoking this function. That evicts the pages from the kernel page
/// cache so that the buffered fallback inside `hash_direct` reads from the FUSE
/// driver → physical disk, restoring the disk-to-disk guarantee. See
/// `fadvise_dontneed()` and the Phase 2 loop in `run()`.
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
fn hash_direct(path: &Path, gen_md5: bool, gen_xxh: bool, pc: &Arc<PauseCancel>) -> io::Result<FileHashes> {
    // Try platform-specific direct I/O first, fall back to buffered on error.
    let result = hash_direct_impl(path, gen_md5, gen_xxh, pc);
    match result {
        Ok(h)  => Ok(h),
        Err(_) => {
            // Fallback: standard buffered read with pipeline.
            // On FUSE filesystems (ntfs-3g, fuse-exfat), O_DIRECT returns EINVAL
            // and we land here. The caller must have already called
            // fadvise_dontneed() on this path to evict the page cache,
            // so this buffered read goes to the FUSE driver → physical disk.
            // On non-FUSE filesystems, syncfs()/sync_all() guarantees that
            // cache == disk, so the buffered read is still correct.
            hash_buffered(path, gen_md5, gen_xxh, pc)
        }
    }
}

/// Platform-specific direct I/O implementation.
fn hash_direct_impl(path: &Path, gen_md5: bool, gen_xxh: bool, pc: &Arc<PauseCancel>) -> io::Result<FileHashes> {
    // Open with cache-bypassing flags — the file is moved into hash_buffered_file()
    // which passes it to the reader thread. No buffer needed here: the pipeline
    // allocates its own per-chunk buffers inside the reader thread.
    let file = open_direct(path)?;
    hash_buffered_file(file, gen_md5, gen_xxh, pc.clone())
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
    pc:      Arc<PauseCancel>,
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
                    if pc.wait_if_paused().is_err() {
                        tx.send(Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))).ok();
                        break;
                    }
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
    pc:      &Arc<PauseCancel>,
) -> io::Result<FileHashes> {
    let file = fs::File::open(path)?;
    hash_buffered_file(file, gen_md5, gen_xxh, pc.clone())
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
    tx:              &Sender<Msg>,
    destinations:    &[PathBuf],
    src_name:        &str,
    src:             &Path,
    total_bytes:     u64,
    meta_entries:    &[(String, metadata::FileMeta)],
    hashes:          &[(FileHashes, bool)],
    gen_csv:         bool,
    gen_pdf:         bool,
    gen_html:        bool,
    gen_md5:         bool,
    gen_xxh:         bool,
    has_verify:      bool,
    comment:         &str,
    settings:        &Settings,
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
            match metadata::write_csv(dst, src_name, src, total_bytes, destinations, &csv_entries, settings, gen_md5, gen_xxh, comment) {
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
            match pdf_report::write_pdf(dst, src_name, src, total_bytes, destinations, &pdf_entries, settings, gen_md5, gen_xxh, comment) {
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
            match html_report::write_html(dst, src_name, src, total_bytes, destinations, &html_entries, settings, gen_md5, gen_xxh, comment) {
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

/// Copy a file with pause/cancel support, using chunked copy_file_range on Linux
/// (64 MiB per call) or a 4 MiB buffered read/write loop on other platforms.
/// `wait_if_paused()` is checked between each chunk.
fn copy_file(src: &Path, dst: &Path, pc: &PauseCancel) -> io::Result<u64> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        let src_file = fs::File::open(src)?;
        unsafe { libc::posix_fadvise(src_file.as_raw_fd(), 0, 0, libc::POSIX_FADV_SEQUENTIAL); }
        let src_mtime = src_file.metadata().ok().and_then(|m| m.modified().ok());
        let dst_file = fs::OpenOptions::new()
            .write(true).create(true).truncate(true)
            .open(dst)?;
        const CHUNK: usize = 64 * 1024 * 1024;
        let src_fd = src_file.as_raw_fd();
        let dst_fd = dst_file.as_raw_fd();
        let mut total = 0u64;
        let mut use_cfr = true;
        'cfr: loop {
            pc.wait_if_paused()
                .map_err(|_| io::Error::new(io::ErrorKind::Interrupted, "cancelled"))?;
            let ret = unsafe {
                libc::copy_file_range(src_fd, std::ptr::null_mut(), dst_fd, std::ptr::null_mut(), CHUNK, 0)
            };
            match ret.cmp(&0) {
                std::cmp::Ordering::Equal   => break,
                std::cmp::Ordering::Greater => { total += ret as u64; }
                std::cmp::Ordering::Less    => {
                    let err = io::Error::last_os_error();
                    let raw = err.raw_os_error().unwrap_or(0);
                    if raw == libc::EXDEV || raw == libc::ENOSYS || raw == libc::EOPNOTSUPP {
                        use_cfr = false;
                        break 'cfr;
                    }
                    return Err(err);
                }
            }
        }
        if !use_cfr {
            drop(src_file);
            drop(dst_file);
            return copy_file_buffered_chunked(src, dst, pc);
        }
        if let Some(mtime) = src_mtime { let _ = dst_file.set_modified(mtime); }
        return Ok(total);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let src_mtime = fs::metadata(src).ok().and_then(|m| m.modified().ok());
        let total = copy_file_buffered_chunked(src, dst, pc)?;
        if let Some(mtime) = src_mtime {
            if let Ok(f) = fs::OpenOptions::new().write(true).open(dst) {
                let _ = f.set_modified(mtime);
            }
        }
        Ok(total)
    }
}

/// Buffered chunked copy fallback — 4 MiB chunks with pause/cancel checks.
fn copy_file_buffered_chunked(src: &Path, dst: &Path, pc: &PauseCancel) -> io::Result<u64> {
    use std::io::{Read, Write};
    let mut src_file = fs::File::open(src)?;
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::io::AsRawFd;
        unsafe { libc::posix_fadvise(src_file.as_raw_fd(), 0, 0, libc::POSIX_FADV_SEQUENTIAL); }
    }
    let src_mtime = src_file.metadata().ok().and_then(|m| m.modified().ok());
    let mut dst_file = fs::OpenOptions::new()
        .write(true).create(true).truncate(true)
        .open(dst)?;
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    let mut total = 0u64;
    loop {
        pc.wait_if_paused()
            .map_err(|_| io::Error::new(io::ErrorKind::Interrupted, "cancelled"))?;
        let n = src_file.read(&mut buf)?;
        if n == 0 { break; }
        dst_file.write_all(&buf[..n])?;
        total += n as u64;
    }
    if let Some(mtime) = src_mtime { let _ = dst_file.set_modified(mtime); }
    Ok(total)
}

/// Returns bytes-per-second from a sliding 2-second window.
/// Returns 0.0 during the initial warmup period.
fn speed_bps(
    done: u64, snap_bytes: &mut u64, snap_time: &mut Instant, global: &Instant,
) -> f64 {
    let elapsed = global.elapsed().as_secs_f64();
    if elapsed < 0.5 || done == 0 { return 0.0; }
    let snap_elapsed = snap_time.elapsed().as_secs_f64();
    if snap_elapsed >= 1.5 {
        let bps = done.saturating_sub(*snap_bytes) as f64 / snap_elapsed;
        *snap_bytes = done;
        *snap_time = Instant::now();
        return bps;
    }
    // Fallback : moyenne cumulative — toujours correcte quelle que soit la taille des fichiers
    done as f64 / elapsed
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

// ── Linux-only helpers: FUSE detection, page-cache eviction, filesystem sync ──

/// Returns `true` if `path` lives on a FUSE-mounted filesystem.
///
/// ## How it works
///
/// Calls `statfs(2)` on the path and checks `statfs.f_type` against
/// `FUSE_SUPER_MAGIC` (0x65735546, defined in `<linux/magic.h>`). Every FUSE
/// filesystem — ntfs-3g, fuse-exfat, sshfs, encfs, s3fs … — reports this magic
/// number, regardless of what actual filesystem the FUSE driver implements.
///
/// ## Why we detect FUSE
///
/// FUSE filesystems have two properties that affect copy correctness/performance:
///
/// 1. **O_DIRECT fails** (EINVAL): `hash_direct()` falls back to buffered I/O,
///    which reads from the kernel page cache instead of the physical disk.
///    Without mitigation, Phase 2 hashes cached RAM, not actual disk data.
///
/// 2. **fsync is expensive**: ntfs-3g serialises every `fsync()` call through
///    the FUSE daemon. The per-file `sync_all()` pattern in Phase 1 makes N×M
///    fsync round-trips (N files × M destinations), each triggering a full
///    cache flush on the FUSE side. On ntfs-3g, this can be 10–100× slower
///    than on a native kernel filesystem.
///
/// ## Return value
///
/// Returns `false` on any error (path not found, stat fails, etc.) — the caller
/// treats a detection failure conservatively as "not FUSE" and uses the standard
/// code path.
#[cfg(target_os = "linux")]
fn is_fuse_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    // Build a NUL-terminated C string from the path bytes for the statfs() call.
    // CString::new() fails if the path contains a NUL byte, which is illegal on
    // Linux anyway — treat that as "not FUSE".
    let c_path = match std::ffi::CString::new(path.as_os_str().as_bytes()) {
        Ok(p)  => p,
        Err(_) => return false,
    };
    // SAFETY: `buf` is fully initialised by statfs() on success. We check the
    // return value before reading any field.
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::statfs(c_path.as_ptr(), &mut buf) };
    if ret != 0 { return false; }
    // FUSE_SUPER_MAGIC = 0x65735546 (little-endian ASCII "UFse").
    // Cast to u64 for cross-architecture safety: f_type is i32 on 32-bit Linux
    // and i64 on 64-bit Linux; the constant 0x65735546 fits in both.
    (buf.f_type as u64) == 0x6573_5546_u64
}

/// Evicts a file's pages from the OS page cache via `posix_fadvise(FADV_DONTNEED)`.
///
/// ## Purpose
///
/// After Phase 1 copy, the destination file's data lives in the kernel page cache
/// (it was just written there). On FUSE filesystems, `O_DIRECT` fails, so
/// `hash_direct()` falls back to standard buffered I/O. Without eviction, that
/// buffered read would return the in-RAM cached copy — hashing RAM, not disk.
///
/// Calling `fadvise_dontneed` before Phase 2 hashing tells the kernel to release
/// those pages. The next `read()` on the file goes to the FUSE driver → physical
/// disk, restoring the disk-to-disk integrity guarantee.
///
/// ## Prerequisite: all dirty pages must be flushed first
///
/// `FADV_DONTNEED` can only evict **clean** (already-flushed) pages. If dirty
/// pages still exist, the kernel ignores the hint for those pages.
/// This is why `syncfs()` is called at the end of Phase 1, before this function:
/// syncfs flushes all dirty pages to disk, leaving them clean and evictable.
///
/// ## Advisory nature
///
/// `posix_fadvise` is a hint — the kernel is free to ignore it. In practice on
/// Linux with a clean page cache, it reliably drops the pages. If eviction fails
/// silently, the subsequent hash may read from cache instead of disk, but the
/// hash result is still bit-accurate (cache == disk after syncfs). The only risk
/// is a false pass if the disk has a hardware write error not reflected in the
/// cache — that risk is the same as without O_DIRECT.
///
/// Errors are silently ignored: this is a best-effort optimisation.
#[cfg(target_os = "linux")]
fn fadvise_dontneed(path: &Path) {
    use std::os::unix::io::AsRawFd;
    // Open the file read-only to obtain a file descriptor.
    // The file must exist at this point (it was just copied in Phase 1).
    if let Ok(f) = fs::File::open(path) {
        // POSIX_FADV_DONTNEED = 4 (linux/fadvise.h):
        //   "The specified data will not be accessed in the near future."
        //   The kernel releases the cached pages for this range.
        //   Offset 0 + length 0 = the entire file.
        unsafe {
            libc::posix_fadvise(f.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
        }
        // The fd is dropped here. The eviction is not synchronous: the kernel
        // processes the hint asynchronously, but in practice pages are evicted
        // before the next read() on a different fd (which hash_direct opens).
    }
}

/// Calls `syncfs(2)` on the filesystem containing `path`, flushing all buffered
/// dirty pages on that filesystem to physical storage.
///
/// ## Why syncfs instead of per-file fsync?
///
/// `sync_all()` (fsync) flushes one specific file. After N files copied to M
/// destinations, the traditional approach makes N×M fsync syscalls. On FUSE
/// filesystems (ntfs-3g), each fsync is forwarded to the FUSE daemon, which
/// serialises it and writes it through — this is extremely slow.
///
/// `syncfs(2)` flushes the **entire filesystem** in one call. The kernel batches
/// all dirty pages for that mount and writes them in one pass, regardless of how
/// many files are involved. For a complete transfer of N files to one FUSE
/// destination, this reduces the number of expensive FUSE daemon round-trips from
/// N to 1.
///
/// ## Durability guarantee
///
/// `syncfs()` provides the same physical-disk durability as `fsync()` for all
/// files currently buffered on the filesystem. After it returns, all data written
/// during Phase 1 is guaranteed to be on physical storage.
///
/// ## Available since Linux 2.6.39 (2011)
///
/// All supported Ubuntu/Debian/Fedora/Arch distributions include this syscall.
/// The `libc` crate exposes it as `libc::syncfs(fd: c_int) -> c_int`.
#[cfg(target_os = "linux")]
fn syncfs_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;
    // Open the directory to obtain a file descriptor for syncfs().
    // `open(path, O_RDONLY)` on a directory is valid on Linux and does not
    // require write permission — we only need the fd to identify the filesystem.
    let dir = fs::File::open(path)?;
    // SAFETY: dir.as_raw_fd() is a valid, open file descriptor.
    let ret = unsafe { libc::syncfs(dir.as_raw_fd()) };
    if ret != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
