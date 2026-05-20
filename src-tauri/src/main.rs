//! # Bartleby — Tauri v2 Rust backend (`main.rs`)
//!
//! This file is the **entry point** of the Rust process. It is responsible for:
//!
//! 1. **Booting the Tauri application** — calling `tauri::Builder` to initialise
//!    the WebView window, register plugins, and start the event loop.
//!
//! 2. **Defining shared application state** — a `struct AppState` that holds
//!    the live user settings and a one-shot channel for interactive prompts.
//!    This state is injected into command handlers by Tauri's dependency-injection
//!    system via `State<AppState>`.
//!
//! 3. **Exposing Tauri commands** — Rust functions that JavaScript can call via
//!    `window.__TAURI__.core.invoke("command_name", { arg1: value1, … })`.
//!    Tauri serialises the arguments from JS to Rust (via JSON + serde) and the
//!    return value from Rust back to JS automatically.
//!
//! 4. **Orchestrating background threads** — when a copy operation starts,
//!    two threads are spawned: one runs the copy engine, the other forwards its
//!    progress messages as Tauri events to the frontend.
//!
//! ## Tauri v2 vs v1 — key differences
//!
//! | Feature              | Tauri v1                        | Tauri v2 (this file)            |
//! |----------------------|---------------------------------|---------------------------------|
//! | Dialog plugin        | Built into `tauri::api::dialog` | Separate `tauri-plugin-dialog`  |
//! | Window handle type   | `tauri::Window`                 | `tauri::WebviewWindow`          |
//! | Permissions system   | `allowlist` in tauri.conf.json  | `capabilities/*.json` files     |
//! | `window.__TAURI__`   | Always exposed                  | Requires `withGlobalTauri:true` |
//! | State management     | `tauri::State<T>` (same API)    | `tauri::State<T>` (same API)    |
//! | Event emission       | `window.emit(…)`                | `window.emit(…)` (same API)     |
//!
//! ## Threading model
//!
//! Tauri command handlers run on a **Tokio async thread pool** — they must not
//! block. Long-running I/O (like copying gigabytes of video files) is offloaded
//! to dedicated OS threads with `std::thread::spawn`.
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │  Tokio worker thread (start_copy command handler)                       │
//! │                                                                         │
//! │   thread::spawn ──► copy_engine::run()   [I/O heavy, blocks on disk]   │
//! │                          │                                              │
//! │                          │  mpsc::Sender<Msg>  (progress, log, done)   │
//! │                          ▼                                              │
//! │   thread::spawn ──► forwarding loop ──► win.emit("copy-progress", …)   │
//! │                                    ──► win.emit("copy-log", …)         │
//! │                                    ──► win.emit("copy-done", …)        │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Communication diagram (Rust ↔ JavaScript)
//!
//! ```text
//! JavaScript (src/main.js)                Rust (this file)
//! ──────────────────────────────────      ────────────────────────────────────
//! invoke("get_settings")          ──────► fn get_settings()  → Settings (JSON)
//! invoke("save_settings", {…})    ──────► fn save_settings() → ()
//! invoke("start_copy",    {…})    ──────► fn start_copy()    → ()
//! invoke("prompt_reply",  {…})    ──────► fn prompt_reply()  → ()
//! invoke("is_system_dark_mode")   ──────► fn is_system_dark_mode() → bool
//! invoke("open_destinations",[…]) ──────► fn open_destinations()  → ()
//!
//! listen("copy-progress", handler) ◄───── win.emit("copy-progress", payload)
//! listen("copy-log",      handler) ◄───── win.emit("copy-log",      payload)
//! listen("copy-done",     handler) ◄───── win.emit("copy-done",     payload)
//! listen("copy-prompt",   handler) ◄───── win.emit("copy-prompt",   payload)
//! ```

// ── Compiler attributes ───────────────────────────────────────────────────────
//
// `#![cfg_attr(condition, attribute)]` is a conditional attribute: it applies
// `attribute` only when `condition` is true at compile time.
//
// `not(debug_assertions)` is true in release builds (i.e. `cargo build --release`)
// and false in debug builds (`cargo build`). Debug assertions are enabled by
// default only in debug mode.
//
// `windows_subsystem = "windows"` tells the Windows linker to create a GUI
// application (WinMain) rather than a console application (main). Without it,
// a black cmd.exe console window would appear behind the Tauri window on Windows.
// We only apply this in release builds so that `println!` and `eprintln!` still
// work in the terminal during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// ── Standard library imports ──────────────────────────────────────────────────
//
// `use` brings items into scope so we can write `Mutex` instead of
// `std::sync::Mutex` every time. This is purely cosmetic — the compiler resolves
// the full path regardless.

use std::sync::{Arc, Mutex, mpsc};
//              ─────  ───
//              │      └─ "multiple producer / single consumer" channels (std::sync::mpsc)
//              └─ Mutual exclusion lock — only one thread can hold a Mutex at a time.
//                 `Mutex<T>` wraps a value T and protects it from concurrent access.

use std::thread;
// `std::thread` provides `thread::spawn(closure)` to start a new OS thread.
// The spawned thread receives ownership of the closure's captured variables
// (because of the `move` keyword we use later).

use std::path::PathBuf;
// `PathBuf` is the owned, heap-allocated counterpart of `Path` (which is borrowed).
// It behaves like a `String` but for filesystem paths, and handles OS-specific
// separators ('/' on Unix, '\\' on Windows) transparently.

use serde::{Serialize, Deserialize};
// `serde` is Rust's de-facto serialisation framework.
// - `Serialize`   → converts a Rust struct to JSON (sent to JavaScript)
// - `Deserialize` → constructs a Rust struct from JSON (received from JavaScript)
// Both traits are derived (auto-implemented) via `#[derive(…)]` macros.

use tauri::{State, Emitter, Manager, Theme};
//           │      └─ The `Emitter` trait adds the `.emit()` method to `WebviewWindow`.
//           │         Without this `use`, the method would not be in scope.
//           └─ `State<T>` is Tauri's dependency-injection wrapper.
//              Declaring `state: State<AppState>` in a command handler causes Tauri
//              to automatically inject the managed `AppState` — no manual wiring needed.

use tauri::WebviewWindow;

// On Linux, gdk/gtk are used in set_window_theme to write _GTK_THEME_VARIANT
// directly on each GDK window so Muffin/Mutter picks up the decoration variant.
#[cfg(target_os = "linux")]
use gdk;
// The Tauri v2 window handle. Renamed from `tauri::Window` in v1.
// It is `Clone` (backed by an `Arc` internally), so it can be sent to other
// threads safely. Used here to call `.emit()` from the forwarding thread.

// ── Module declarations ───────────────────────────────────────────────────────
//
// `mod name;` tells the Rust compiler: "there is a file called `name.rs` (or
// `name/mod.rs`) next to this file; compile it as a submodule of this crate."
// Items in a submodule are accessed as `name::Item` unless brought into scope with `use`.
//
// The four modules of Bartleby's backend:
mod copy_engine;   // Phase 1 (streaming copy) + Phase 2 (hash verify) + Phase 3 (reports)
mod metadata;      // mediainfo-based technical metadata extraction + CSV writing
mod pdf_report;    // A4 landscape PDF report generation (thumbnails, table, header)
mod html_report;   // Self-contained HTML report generation (thumbnails, table, header)
mod mhl_report;    // ASC MHL v2.0 hash-list generator (one file per destination)
mod settings;      // User preferences: JSON persistence, defaults, typed fields
mod sidecar;       // Resolves bundled sidecar binaries (mediainfo, ffmpeg) with PATH fallback
mod verify_engine; // Checksum / MHL verification + post-verify MHL generation

// Bring `Settings` into scope from the `settings` module.
// We use it often enough that the full path `settings::Settings` would be noisy.
use settings::Settings;

// ── Application version ───────────────────────────────────────────────────────

/// Application version string, derived at compile time from `Cargo.toml`.
///
/// `env!("CARGO_PKG_VERSION")` is expanded by the compiler to the `version`
/// field in Cargo.toml, so this constant never needs manual updating — keep
/// Cargo.toml and tauri.conf.json in sync and VERSION follows automatically.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// ── Shared application state ──────────────────────────────────────────────────

/// State that is shared across all Tauri command handler invocations.
///
/// Tauri wraps this struct in an `Arc<AppState>` internally (via `.manage(…)`)
/// and injects a reference into any command handler that declares
/// `state: State<AppState>` as a parameter.
///
/// Because multiple command handlers may run **concurrently** on different Tokio
/// worker threads, every field that can be mutated must be protected by a
/// synchronisation primitive. Here we use `Mutex<T>`:
///
/// ```text
/// Thread A: get_settings()   → locks settings, reads, unlocks
/// Thread B: save_settings()  → waits for lock, then writes + saves
/// ```
///
/// ### Why `Mutex` and not `RwLock`?
/// `RwLock` allows concurrent reads but exclusive writes. In practice:
/// - `settings` is read at most once per copy start and written on every checkbox
///   toggle — reads and writes are rare enough that the `RwLock` optimisation
///   would not provide measurable benefit.
/// - `reply_tx` is always written (replaced), never read without immediately sending.
/// Using `Mutex` keeps the code simpler.
///
/// ### Why is this struct not `pub`?
/// Rust defaults to private visibility for items not marked `pub`. `AppState` is
/// only ever constructed in `main()` and referenced via `State<AppState>` in the
/// same file. No other crate or module needs to name its type directly.
struct AppState {
    /// The live user preferences, protected by a mutex.
    ///
    /// `Mutex<T>` provides interior mutability: even through a shared `&AppState`
    /// reference, we can obtain a mutable `&mut Settings` by locking the mutex.
    /// This is safe because the mutex guarantees that only one thread can hold
    /// the mutable reference at a time — the Rust borrow checker cannot enforce
    /// this at compile time across threads, so the runtime lock does it instead.
    settings: Mutex<Settings>,

    /// Pause/cancel handle for the active copy operation.
    /// `None` when no copy is running.
    pause_cancel: Mutex<Option<Arc<copy_engine::PauseCancel>>>,

    /// Pause/cancel handle for an active verification.
    /// `None` when no verification is running.
    verify_pc: Mutex<Option<Arc<copy_engine::PauseCancel>>>,

    /// One-shot reply channel for interactive copy-engine prompts.
    ///
    /// ### Lifecycle
    /// 1. `start_copy` creates an `mpsc::sync_channel(1)` and stores the **sender**
    ///    end (`SyncSender`) here, inside `Some(…)`.
    /// 2. When the copy engine encounters a non-empty destination or a file conflict,
    ///    it sends a `Msg::NonEmptyDest` or `Msg::Conflicts` and then **blocks**,
    ///    waiting for a `Reply` on the **receiver** end.
    /// 3. The forwarding thread relays the message to JavaScript as a `copy-prompt`
    ///    Tauri event. JavaScript displays a dialog to the user.
    /// 4. The user clicks a button; JavaScript calls `invoke("prompt_reply", …)`.
    /// 5. `prompt_reply` pops the `SyncSender` from this field and sends the `Reply`,
    ///    which unblocks the copy engine thread.
    ///
    /// ### Why `Option<SyncSender<…>>`?
    /// - `None` → no copy operation is in progress (startup state, or between copies).
    /// - `Some(tx)` → a copy is running and the engine may prompt the user.
    /// Using `Option` makes the "no copy in progress" state explicit and avoids
    /// a dummy channel being allocated when the app starts.
    ///
    /// ### `SyncSender` vs `Sender`
    /// `mpsc::channel()` creates an **unbounded** channel with an async `Sender`.
    /// `mpsc::sync_channel(n)` creates a **bounded** channel (capacity = n) with a
    /// `SyncSender` that blocks if the buffer is full. With capacity = 1, the send
    /// in `prompt_reply` is always non-blocking in practice (the engine consumes the
    /// reply immediately), but the bounded buffer avoids any dynamic allocation.
    reply_tx: Mutex<Option<mpsc::SyncSender<copy_engine::Reply>>>,
}

// ── Application entry point ───────────────────────────────────────────────────

/// Tauri application bootstrap — the first Rust function the OS calls.
///
/// `fn main()` is the conventional name for the program entry point in Rust.
/// The OS calls it when the binary is executed.
///
/// ### Builder pattern
/// `tauri::Builder::default()` returns a `Builder` struct with safe defaults.
/// Each `.method()` call configures one aspect of the application and returns
/// the same `Builder` so calls can be chained (the "builder pattern"). The chain
/// terminates with `.run(…)` which takes ownership of the builder, creates the
/// WebView window, and enters the event loop. It never returns under normal
/// operation.
///
/// ### `.plugin(tauri_plugin_dialog::init())`
/// Registers the dialog plugin, which exposes
/// `window.__TAURI__.dialog.open({ directory: true })` in JavaScript.
/// Without this line, the dialog API is unavailable in JS and any call to it
/// will throw a runtime error. Plugins in Tauri v2 must be explicitly registered.
///
/// ### `.manage(AppState { … })`
/// Hands the `AppState` value to Tauri, which wraps it in an `Arc<AppState>`
/// and stores it in the application. From this point on, any command handler
/// that declares `state: State<AppState>` receives an `Arc`-backed reference
/// to the same instance — no cloning of the data, just cloning the `Arc` pointer.
///
/// ### `tauri::generate_handler![…]`
/// A macro that generates the internal dispatch table Tauri uses to route
/// `invoke("name", args)` calls from JavaScript to the correct Rust function.
/// Every function listed here must be annotated with `#[tauri::command]`.
/// Functions not listed here cannot be called from JS (they are invisible).
///
/// ### `tauri::generate_context!()`
/// A macro that reads `tauri.conf.json` **at compile time** and embeds the
/// application metadata (name, version, identifier, asset paths) into the binary.
/// It does not perform any I/O at runtime.
///
/// ### `.expect("…")`
/// `.run()` returns `Result<(), tauri::Error>`. `.expect(msg)` panics with `msg`
/// if the result is `Err(…)`. This is appropriate here because a failure to start
/// the event loop is a fatal, unrecoverable error — there is nothing sensible to
/// do except crash with a clear message.
fn main() {
    // ── Linux: set GTK_THEME before Tauri initialises GTK ────────────────────
    // Must happen before tauri::Builder::default() which calls gtk_init().
    // Silently ignored if settings file is missing or unparseable.
    #[cfg(target_os = "linux")]
    apply_gtk_theme_from_settings();

    tauri::Builder::default()
        // Register the dialog plugin (enables window.__TAURI__.dialog in JavaScript).
        // Without this, folder picker dialogs cannot be opened from JS.
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())

        // Register our AppState as Tauri-managed shared state.
        // Tauri wraps it in Arc<AppState> and injects it into command handlers.
        .manage(AppState {
            // Load settings from disk at startup (falls back to defaults if absent).
            settings: Mutex::new(Settings::load()),
            // No copy is running yet.
            pause_cancel: Mutex::new(None),
            // No verification is running yet.
            verify_pc: Mutex::new(None),
            // No copy is running yet, so no reply channel exists.
            reply_tx: Mutex::new(None),
        })

        // Register all Tauri commands callable from JavaScript.
        // `generate_handler!` is a macro: it expands to the glue code that
        // deserialises JS arguments, calls the Rust function, and serialises the return value.
        .invoke_handler(tauri::generate_handler![
            get_settings,
            save_settings,
            start_copy,
            prompt_reply,
            pause_copy,
            resume_copy,
            cancel_copy,
            is_system_dark_mode,
            open_destinations,
            send_notification,
            save_log,
            get_home_dir,
            get_app_version,
            get_volume_info,
            set_window_theme,
            open_verifier_window,
            parse_verification_file,
            start_verification,
            pause_verification,
            resume_verification,
            cancel_verification,
            save_verify_html,
            generate_post_verify_mhl,
        ])

        // ── Application setup — runs once during initialisation ───────────────
        // Creating a second WebView2 window from a command at runtime is
        // unreliable on Windows (the event loop deadlocks while WebView2's COM
        // callbacks wait for it). Creating it here, during init — the same phase
        // in which the main window is created — is reliable on every platform.
        .setup(|app| {
            let handle = app.handle().clone();

            // The verification window is created once, hidden. It is shown on
            // demand by `open_verifier_window` and hidden again (not destroyed)
            // when closed, so it is never re-created at runtime.
            match tauri::WebviewWindowBuilder::new(
                &handle,
                "verifier",
                tauri::WebviewUrl::App("verifier.html".into()),
            )
            .title("Bartleby — Verification")
            .inner_size(980.0, 700.0)
            .min_inner_size(700.0, 500.0)
            .resizable(true)
            .visible(false)
            .center()
            .build()
            {
                Ok(verifier) => {
                    let h = handle.clone();
                    verifier.on_window_event(move |event| {
                        if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                            // Hide instead of destroy — keeps re-opening instant
                            // and avoids the fragile runtime re-creation path.
                            api.prevent_close();
                            if let Some(w) = h.get_webview_window("verifier") {
                                let _ = w.hide();
                            }
                        }
                    });
                }
                Err(e) => eprintln!("Bartleby: could not create verifier window: {e}"),
            }

            // Closing the main window quits the whole application. Without this,
            // the still-existing (hidden) verifier window keeps the process
            // alive after the main window is closed.
            if let Some(main_win) = app.get_webview_window("main") {
                let h = handle.clone();
                main_win.on_window_event(move |event| {
                    if let tauri::WindowEvent::CloseRequested { .. } = event {
                        h.exit(0);
                    }
                });
            }

            Ok(())
        })

        // Start the Tauri event loop. This call blocks until the window is closed.
        // `generate_context!()` reads tauri.conf.json at compile time.
        .run(tauri::generate_context!())

        // Panic with a clear message if the event loop fails to start.
        // `.expect()` is equivalent to: if Err(e) { panic!("error while running Bartleby: {}", e) }
        .expect("error while running Bartleby");
}

// ── Tauri commands ────────────────────────────────────────────────────────────
//
// A Tauri command is a Rust function that can be called from JavaScript.
// Annotate it with `#[tauri::command]` — this macro:
//   1. Generates a wrapper function that Tauri's dispatcher can call.
//   2. Handles JSON deserialisation of arguments from JS.
//   3. Handles JSON serialisation of the return value back to JS.
//
// On the JavaScript side:
//   const result = await window.__TAURI__.core.invoke("function_name", { arg: value });
//
// Key rules:
//   • Parameter names in JS must match Rust parameter names in snake_case.
//     Tauri also accepts camelCase JS keys and converts them automatically.
//   • Parameters injected by Tauri (State<T>, Window, AppHandle…) must NOT be
//     passed from JS — Tauri injects them automatically and JS is unaware of them.
//   • Return type `Result<T, String>` → resolves the JS Promise on Ok,
//     rejects it (throws in await) on Err.
//   • Return type `T` (no Result) → always resolves.

/// Returns a clone of the current user settings.
///
/// Called by `main.js` at startup to populate all UI controls:
/// checkboxes, text fields, and the active colour theme.
///
/// ### Why `.clone()` instead of returning a reference?
/// Tauri serialises the return value to JSON to send it across the Rust→JS
/// boundary. Serialisation requires the value (or a reference to it), but the
/// `MutexGuard` that gives us access to the `Settings` is only valid while the
/// mutex is locked. Returning a reference would keep the mutex locked until the
/// JavaScript side finishes — which is impossible across the IPC boundary.
/// Instead, we call `.clone()` to produce an independent copy of the settings,
/// then drop the guard (which releases the lock). The clone is cheap because
/// `Settings` only contains `String` and `bool` values.
///
/// ### `.lock().unwrap()`
/// `Mutex::lock()` returns `Result<MutexGuard<T>, PoisonError<…>>`.
/// A `PoisonError` occurs only if another thread panicked while holding the lock
/// (a "poisoned mutex"). In normal operation this never happens. `.unwrap()` is
/// therefore safe here — it would only panic in the presence of a prior bug.
#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    // 1. Acquire the mutex lock → get a MutexGuard<Settings>
    // 2. Call .clone() on the Settings inside the guard
    // 3. The guard is dropped here (end of expression) → mutex is unlocked
    state.settings.lock().unwrap().clone()
}

/// Replaces the current settings with `new_settings` and persists them to disk.
///
/// Called from `main.js` when the user changes a checkbox or saves the settings
/// dialog. The new settings take effect immediately for all subsequent operations.
///
/// ### `new_settings: Settings`
/// The argument is received by **value** (ownership transferred from Tauri's
/// deserialiser). Tauri deserialised it from the JSON payload sent by JavaScript.
///
/// ### `*s = new_settings` — dereference assignment
/// `state.settings.lock().unwrap()` returns a `MutexGuard<Settings>`.
/// `MutexGuard<T>` implements the `DerefMut` trait, which means it can be
/// treated as a `&mut T` (a mutable reference to the inner value).
/// The `*` dereferences the guard to get a `Settings`, and `= new_settings`
/// replaces that entire value in one assignment — equivalent to `*ptr = val` in C.
///
/// ### `Ok(())` — the unit return
/// `()` (pronounced "unit") is Rust's equivalent of `void`. `Result<(), String>`
/// signals success with no meaningful value. JavaScript receives a resolved Promise.
#[tauri::command]
fn save_settings(state: State<AppState>, new_settings: Settings) -> Result<(), String> {
    // Lock the mutex → get exclusive mutable access to the Settings
    let mut s = state.settings.lock().unwrap();
    // Replace the entire Settings value in one move (no field-by-field copy)
    *s = new_settings;
    // Write the new settings to ~/.config/bartleby/settings.json
    // (errors are silently discarded inside save() — see settings.rs)
    s.save();
    // Signal success to JavaScript (the await in JS resolves without a value)
    Ok(())
}

// ── Event payload structs ──────────────────────────────────────────────────────
//
// These structs are the data "envelopes" carried by Tauri events emitted from Rust
// to JavaScript via `win.emit("event-name", payload)`.
//
// ### How the JS side receives them
// In JavaScript:
//   window.__TAURI__.event.listen("copy-progress", event => {
//       const { fraction, label } = event.payload; // payload is the serialised struct
//   });
//
// ### `#[derive(Clone)]`
// `win.emit()` takes ownership of the payload. If we needed to emit the same
// payload to multiple windows, we would need to clone it. Tauri requires the
// payload type to be `Clone` even for single-window apps (it may clone internally
// for routing purposes). Deriving `Clone` auto-generates a method that copies all
// fields.
//
// ### `#[derive(Serialize)]`
// Tauri serialises the payload struct to JSON before sending it over the IPC
// bridge to JavaScript. `serde::Serialize` is auto-implemented by this derive
// macro. The JSON keys match the Rust field names (snake_case by default).
//
// ### Inline struct fields
// Fields are written on one line here (e.g. `struct LogPayload { line: String }`)
// because each struct is small and purpose-specific. Both styles are valid Rust.

/// Payload for `copy-progress` events: progress bar fraction and label text.
#[derive(Clone, Serialize)]
struct ProgressPayload {
    /// Progress fraction in [0.0, 1.0]. JavaScript multiplies by 100 for percentage.
    fraction: f64,
    /// Text shown below the progress bar: current filename + optional ETA string.
    label:    String,
}

/// Payload for `copy-log` events: one line of text to append to the log panel.
#[derive(Clone, Serialize)]
struct LogPayload {
    /// A single log line, always terminated with `\n`.
    line: String,
}

/// Payload for `copy-done` events: final operation result.
#[derive(Clone, Serialize)]
struct DonePayload {
    /// `true` = all files copied (and verified) successfully.
    /// `false` = at least one copy error or MD5 mismatch occurred.
    ok:      bool,
    /// One-line human-readable summary, shown in the UI status label.
    summary: String,
}

/// One entry in the conflict table shown to the user.
#[derive(Clone, Serialize)]
struct ConflictItemPayload {
    rel_path:   String,
    size_match: bool,
    date_match: bool,
}

/// Payload for `copy-prompt` events: requests the user to make a decision.
///
/// The copy engine is **blocked** waiting for a reply when this event is emitted.
/// JavaScript must call `invoke("prompt_reply", { reply: "continue"|"skip"|"cancel" })`
/// to unblock it.
#[derive(Clone, Serialize)]
struct PromptPayload {
    /// Discriminator for the dialog type:
    /// - `"non_empty"` → one or more destinations already contain files.
    /// - `"conflicts"` → one or more source files already exist in the destination.
    kind:  String,
    /// Destination paths for `non_empty` prompts. Empty for `conflicts`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    items: Vec<String>,
    /// Structured conflict data for `conflicts` prompts. Absent for `non_empty`.
    #[serde(skip_serializing_if = "Option::is_none")]
    conflict_items: Option<Vec<ConflictItemPayload>>,
}

// ── start_copy ────────────────────────────────────────────────────────────────

/// Arguments deserialised from the JavaScript `invoke("start_copy", args)` call.
///
/// `#[derive(Deserialize)]` auto-generates the code that converts the JSON object
/// sent by JavaScript into this Rust struct. Tauri calls that generated code before
/// invoking `start_copy`.
///
/// ### Field naming
/// Rust uses snake_case by default. JavaScript typically uses camelCase. Tauri
/// accepts both: `gen_md5` in Rust matches both `gen_md5` and `genMd5` in JS.
///
/// ### `#[allow(dead_code)]`
/// `open_dest` is sent by JavaScript but never read in Rust — the "open destinations"
/// logic lives entirely in the JavaScript `copy-done` event handler. Without this
/// attribute the Rust compiler would emit a `dead_code` warning for `open_dest`.
/// `allow(dead_code)` is the idiomatic way to suppress such warnings on fields that
/// are intentionally unused on one side of an API boundary.
#[derive(Deserialize)]
struct StartCopyArgs {
    /// Absolute path to the source directory (e.g. `/media/usb/SHOOT_2024`).
    src:          String,
    /// Absolute paths to one or more destination directories.
    destinations: Vec<String>,
    /// Hash algorithm to use for integrity verification and checksum file generation.
    /// One of: "none", "size", "md5", "sha1", "xxh64", "xxh3", "xxh128", "c4".
    hash_algo:    String,
    /// Generate a `{src_name}_report.csv` metadata table in each destination.
    gen_csv:      bool,
    /// Generate a `{src_name}_report.pdf` visual report in each destination.
    gen_pdf:      bool,
    /// Generate a self-contained `{src_name}_report.html` report in each destination.
    gen_html:     bool,
    /// Copy the source folder itself into the destination, not just its contents.
    /// When true:  destination/source_name/file.ext
    /// When false: destination/file.ext  (current default)
    copy_as_subfolder: bool,
    /// Generate an ASC MHL v2.0 hash list in each destination.
    /// Ignored when hash_algo is "none" or "size".
    #[serde(default)]
    gen_mhl:      bool,
    /// Per-job comment/note written into report headers (CSV, PDF, HTML).
    /// HTML string from the WYSIWYG editor (bold/italic/underline only).
    #[serde(default)]
    comment:      String,
    /// Plain-text note written only into the MHL `<comment>` field.
    /// Kept separate from `comment` so the MHL stays lightweight.
    #[serde(default)]
    mhl_comment:  String,
    /// Per-job shooting location written into report headers and MHL.
    #[serde(default)]
    location:     String,
    /// Whether to open each destination in the file manager after a successful copy.
    /// This flag is read by JavaScript in the `copy-done` handler, not by Rust.
    #[allow(dead_code)]
    open_dest:    bool,
}

/// Launches the file transfer pipeline in two background threads and returns immediately.
///
/// This command is the **heart of the application**. Its job is:
/// 1. Validate the input (non-empty source and destinations).
/// 2. Create the communication channels between threads.
/// 3. Spawn the copy engine thread (does the actual work).
/// 4. Spawn the forwarding thread (relays progress to JavaScript).
/// 5. Return `Ok(())` — the operation continues asynchronously.
///
/// ### Why return immediately?
/// Tauri command handlers run on Tokio's async thread pool. If `start_copy` blocked
/// (e.g. by running the copy itself), it would consume a Tokio worker thread for
/// the entire duration of the transfer — potentially minutes or hours. Other commands
/// (`prompt_reply`, `get_settings`) could not be served in the meantime. By spawning
/// dedicated OS threads and returning immediately, the Tokio thread is freed for other
/// work instantly.
///
/// ### Thread ownership and `move` closures
/// In Rust, each value has exactly one owner at any given time. When a thread is
/// spawned with `thread::spawn(move || { … })`, the `move` keyword causes the closure
/// to **take ownership** of all variables it references from the enclosing scope.
/// After the `spawn`, those variables no longer exist in `start_copy` — they belong
/// to the new thread. This is how Rust prevents data races at compile time without
/// locks: if only one thread owns the data, no synchronisation is needed.
///
/// ### `mpsc` channels — Multiple Producer, Single Consumer
/// `mpsc::channel()` creates a pair `(Sender<T>, Receiver<T>)`:
/// - `Sender<T>` can be cloned and moved to multiple threads (the "multiple producers").
/// - `Receiver<T>` cannot be cloned — only one thread can read from it (the "single consumer").
/// - Sending is non-blocking (unless the channel is full — which unbounded channels never are).
/// - Receiving blocks until a message is available.
/// - If all `Sender` instances are dropped, `recv()` returns `Err(RecvError)` immediately.
///
/// ### `mpsc::sync_channel(1)` for the reply channel
/// Unlike the regular `channel()`, `sync_channel(n)` creates a **bounded** channel
/// that can buffer at most `n` messages. With `n = 1`:
/// - The `SyncSender` can send one message without blocking.
/// - A second send blocks until the receiver has consumed the first.
/// In our case, the engine always waits for a reply before prompting again,
/// so the buffer never fills — but `sync_channel(1)` avoids any heap allocation
/// for the buffer slot (it is pre-allocated on creation).
#[tauri::command]
fn start_copy(
    window: WebviewWindow,   // Tauri v2 window handle; used to emit events to JavaScript.
                             // Injected automatically by Tauri — not sent from JS.
    state:  State<AppState>, // Shared application state; injected by Tauri.
    args:   StartCopyArgs,   // Deserialised from the JSON payload sent by JavaScript.
) -> Result<(), String> {

    // ── Input validation ───────────────────────────────────────────────────────
    // Return early with a descriptive error string if the inputs are invalid.
    // Returning `Err(String)` causes Tauri to reject the JavaScript Promise,
    // which surfaces as a thrown exception in the `await` call.
    if args.src.is_empty() {
        return Err("Please choose a source directory.".into());
        // `.into()` converts `&str` to `String`. The `Err(…)` variant of `Result`
        // requires an owned `String` here (not a borrowed `&str`).
    }
    if args.destinations.is_empty() {
        return Err("Please add at least one destination.".into());
    }

    // ── Path conversion: String → PathBuf ─────────────────────────────────────
    // JavaScript sends paths as plain strings (e.g. "/media/usb/SHOOT_2024").
    // Rust prefers `PathBuf` for filesystem paths because it:
    //   • Handles OS-specific separators automatically.
    //   • Provides type-safe path manipulation (.join(), .parent(), .extension()…).
    //   • Prevents accidentally treating a path as arbitrary text.
    let src_path = PathBuf::from(&args.src);

    // `.iter()` : iterate over &String references.
    // `.map(PathBuf::from)` : convert each &String to a PathBuf (via `From<&String>`).
    //   `PathBuf::from` is a function pointer here — shorthand for `|s| PathBuf::from(s)`.
    // `.collect()` : gather results into a new Vec<PathBuf>.
    //   The type annotation `Vec<PathBuf>` on the left tells `collect` which collection to build.
    let dst_paths: Vec<PathBuf> = args.destinations.iter().map(PathBuf::from).collect();

    // ── Settings snapshot ──────────────────────────────────────────────────────
    // Take a clone of the current settings before spawning threads.
    // Rationale: the copy engine needs the settings throughout its entire run
    // (for report header, active columns, etc.). We cannot lend a reference to
    // `AppState.settings` to another thread because:
    //   a) The borrow would need to outlive `start_copy`, violating the borrow rules.
    //   b) The Mutex lock would be held for the entire duration — blocking UI changes.
    // A cheap clone avoids both issues: the engine gets its own copy, the Mutex is
    // unlocked immediately after `.clone()`.
    let settings_snapshot = state.settings.lock().unwrap().clone();

    // ── Create communication channels ─────────────────────────────────────────
    // Channel 1: copy engine → forwarding thread (unbounded, async).
    // The copy engine can send as many messages as it wants without blocking.
    let (tx, rx) = mpsc::channel::<copy_engine::Msg>();
    //  ──  ──
    //  │   └─ Receiver<Msg> — given to the forwarding thread
    //  └─ Sender<Msg> — given to the copy engine thread

    // Channel 2: forwarding thread ← JS → `prompt_reply` → copy engine (bounded, capacity 1).
    // The copy engine blocks on `reply_rx.recv()` after sending a prompt.
    // `prompt_reply` sends the user's answer on `reply_tx`, unblocking the engine.
    let (reply_tx, reply_rx) = mpsc::sync_channel::<copy_engine::Reply>(1);
    //             ────────
    //             Receiver<Reply> — given to the copy engine thread

    // Store the SyncSender in AppState so `prompt_reply` can retrieve it later.
    // `*state.reply_tx.lock().unwrap()` = dereference the MutexGuard to get
    // `Option<SyncSender<Reply>>`, then assign `Some(reply_tx)` to replace `None`.
    *state.reply_tx.lock().unwrap() = Some(reply_tx);

    // Create the pause/cancel handle and store it in AppState.
    let pc = copy_engine::PauseCancel::new();
    *state.pause_cancel.lock().unwrap() = Some(pc.clone());

    // ── Extract fields before moving into closures ─────────────────────────────
    let hash_algo = copy_engine::HashAlgo::from_str(&args.hash_algo);
    let gen_csv   = args.gen_csv;
    let gen_pdf   = args.gen_pdf;
    let gen_html  = args.gen_html;
    let gen_mhl   = args.gen_mhl;
    let comment     = args.comment;
    let mhl_comment = args.mhl_comment;
    let location    = args.location;

    // ── Thread 1: Copy engine ──────────────────────────────────────────────────
    // `thread::spawn(move || { … })` starts a new OS thread.
    //
    // `move` captures all referenced variables by MOVING them into the closure:
    //   - `src_path`, `dst_paths`, `settings_snapshot` → owned by this thread
    //   - `tx` (Sender<Msg>) → the engine sends its progress here
    //   - `reply_rx` (Receiver<Reply>) → the engine waits for prompt replies here
    //   - `verify`, `gen_md5`, `gen_csv`, `gen_pdf` → copied (bool is Copy)
    //
    // After this `spawn`, `src_path`, `dst_paths`, `tx`, and `reply_rx` can no longer
    // be used in `start_copy` — ownership has been transferred to the new thread.
    // The compiler enforces this and will reject any attempt to use them afterwards.
    //
    // `copy_engine::run` is a long-running synchronous function (seconds to minutes).
    // It runs the three phases: copy → verify → reports.
    thread::spawn(move || {
        copy_engine::run(
            src_path,
            dst_paths,
            hash_algo,
            gen_csv,
            gen_pdf,
            gen_html,
            gen_mhl,
            args.copy_as_subfolder,
            comment,
            mhl_comment,
            location,
            settings_snapshot,
            tx,
            reply_rx,
            pc,
        );
        // When run() returns, `tx` is dropped. This signals the forwarding thread
        // that no more messages will arrive (Receiver::recv() will return Err).
    });

    // ── Thread 2: Event forwarding ─────────────────────────────────────────────
    // This thread's sole job is to translate `copy_engine::Msg` variants into
    // Tauri events that JavaScript can receive via `listen("event-name", …)`.
    //
    // ### Why not emit directly from the copy engine?
    // `copy_engine::run` is designed as pure I/O logic with no Tauri dependency.
    // It knows nothing about `WebviewWindow` or Tauri events. Keeping it pure:
    //   • Makes it testable without a Tauri runtime.
    //   • Allows the message format to evolve independently of the UI.
    //   • Separates the "what happened" (Msg) from "how to tell the UI" (emit).
    //
    // `window.clone()` clones the Arc-backed window handle — zero-cost pointer copy.
    // We must clone before the `move` closure below consumes `window`.
    let win = window.clone();

    thread::spawn(move || {
        // `loop` is an infinite loop in Rust (equivalent to `while true` in C/JS).
        // We exit via `break` when `Done` or `Err(_)` is received.
        loop {
            // `rx.recv()` blocks this thread until a message is available.
            // Returns:
            //   Ok(Msg)       → a message from the copy engine
            //   Err(RecvError) → all Sender instances have been dropped (copy thread ended)
            //
            // `match` performs exhaustive pattern matching — every possible variant
            // of the result must be handled, or the compiler rejects the code.
            match rx.recv() {

                // ── Progress update ───────────────────────────────────────────
                // Sent on every 1 MiB chunk read. JavaScript uses this to update
                // the progress bar. The event name "copy-progress" must match
                // exactly what JS listens for: listen("copy-progress", …).
                Ok(copy_engine::Msg::Progress(f, label)) => {
                    // `let _ = …` discards the Result from `.emit()`.
                    // A send failure means the window was closed — not actionable here.
                    let _ = win.emit("copy-progress", ProgressPayload { fraction: f, label });
                }

                // ── Log line ──────────────────────────────────────────────────
                // One line of text to append to the scrollable log panel in the UI.
                Ok(copy_engine::Msg::Log(line)) => {
                    let _ = win.emit("copy-log", LogPayload { line });
                }

                // ── Operation complete ────────────────────────────────────────
                // Emitted once at the very end of `copy_engine::run`.
                // `ok = false` if any file failed to copy or verify.
                // After emitting, we `break` out of the loop — this thread's job is done.
                Ok(copy_engine::Msg::Done(ok, summary)) => {
                    let _ = win.emit("copy-done", DonePayload { ok, summary });
                    break; // ← exits the loop; the thread function returns; thread ends
                }

                // ── Non-empty destination prompt ──────────────────────────────
                // The copy engine is BLOCKED on `reply_rx.recv()`, waiting for
                // the user's decision. We relay the prompt to JavaScript, which
                // will display a dialog. When the user clicks a button, JS calls
                // `invoke("prompt_reply", { reply: "continue" | "skip" | "cancel" })`.
                Ok(copy_engine::Msg::NonEmptyDest(paths)) => {
                    let _ = win.emit("copy-prompt", PromptPayload {
                        kind:           "non_empty".into(),
                        items:          paths,
                        conflict_items: None,
                    });
                }

                // ── File conflict prompt ──────────────────────────────────────
                // Same mechanism as NonEmptyDest. The engine is blocked; JS
                // shows a scrollable conflict table with size+date match indicators.
                Ok(copy_engine::Msg::Conflicts(infos)) => {
                    let conflict_items: Vec<ConflictItemPayload> = infos.into_iter()
                        .map(|ci| ConflictItemPayload {
                            rel_path:   ci.rel_path,
                            size_match: ci.size_match,
                            date_match: ci.date_match,
                        })
                        .collect();
                    let _ = win.emit("copy-prompt", PromptPayload {
                        kind:           "conflicts".into(),
                        items:          vec![],
                        conflict_items: Some(conflict_items),
                    });
                }

                // ── MHL conflict prompt (Phase 3) ─────────────────────────────
                // A previous MHL for this source was found at the destination.
                // Reply::Continue → replace, Reply::Skip → keep both, Reply::Cancel → skip MHL.
                Ok(copy_engine::Msg::MhlConflict { dst, existing_mhl }) => {
                    let _ = win.emit("copy-prompt", PromptPayload {
                        kind:           "mhl_conflict".into(),
                        items:          vec![dst, existing_mhl],
                        conflict_items: None,
                    });
                }

                // ── Channel closed unexpectedly ───────────────────────────────
                // All Sender<Msg> instances were dropped without sending Msg::Done.
                // This can happen if the copy thread panicked. We emit a Done(false)
                // so the UI resets to its idle state instead of spinning forever.
                Err(_) => {
                    let _ = win.emit("copy-done", DonePayload {
                        ok:      false,
                        summary: "Operation ended unexpectedly.".into(),
                    });
                    break;
                }
            } // end match
        } // end loop
        // When this closure returns, `win` (the cloned WebviewWindow) is dropped.
        // The original `window` in `start_copy` was also dropped when start_copy returned.
        // Dropping all clones does NOT close the actual window — the real window
        // is owned by the Tauri runtime and lives until the user closes it.
    });

    // Return immediately. The copy is running in the background.
    // JavaScript receives a resolved Promise with no value (`undefined`),
    // then waits for "copy-progress" / "copy-done" events via `listen()`.
    Ok(())
}

/// Delivers the user's response to the copy engine's interactive prompt.
///
/// This command is called from JavaScript when the user clicks "Continue",
/// "Skip", or "Cancel" in a non-empty-destination or file-conflict dialog.
///
/// ### Flow
/// ```text
/// copy engine: sends Msg::NonEmptyDest(…) → blocks on reply_rx.recv()
/// forwarding:  receives Msg, emits "copy-prompt" event to JS
/// JavaScript:  shows dialog, user clicks "Continue"
/// JavaScript:  invoke("prompt_reply", { reply: "continue" })
/// prompt_reply: maps "continue" → Reply::Continue, sends on reply_tx
/// copy engine: reply_rx.recv() returns Ok(Reply::Continue) → continues
/// ```
///
/// ### `.as_ref()` on the `MutexGuard`
/// `state.reply_tx.lock().unwrap()` returns a `MutexGuard<Option<SyncSender<Reply>>>`.
/// We need to call `.send()` on the `SyncSender` without taking ownership of it
/// (so it stays in `AppState` for potential future prompts in the same copy).
/// `.as_ref()` converts `&Option<T>` to `Option<&T>` — a reference to the inner value,
/// not ownership of it. We can then call `.send()` through the shared reference.
///
/// ### `if let Some(tx) = …`
/// If no copy is in progress, `reply_tx` is `None` and this is a no-op.
/// `if let Some(x) = option_value` is the idiomatic way to unpack an `Option`
/// and execute a block only when the value is `Some(x)`.
#[tauri::command]
fn prompt_reply(state: State<AppState>, reply: String) -> Result<(), String> {
    // Map the JavaScript string to the Rust enum variant.
    // `match` is exhaustive: the `_` arm catches any unexpected values (e.g. typos
    // or future additions in JS) and treats them as "Cancel" for safety.
    let r = match reply.as_str() {
        // `.as_str()` converts &String to &str so we can match string literals
        "continue" => copy_engine::Reply::Continue,
        "skip"     => copy_engine::Reply::Skip,
        _          => copy_engine::Reply::Cancel, // default: cancel on unknown input
    };
    // Acquire the lock, get an Option<&SyncSender<Reply>>, and send if Some.
    // `let _ = tx.send(r)` discards the Result — a send failure here means the
    // copy thread has already finished (Reply would be spurious anyway).
    if let Some(tx) = state.reply_tx.lock().unwrap().as_ref() {
        let _ = tx.send(r);
    }
    Ok(())
}

// ── Transport controls ────────────────────────────────────────────────────────

/// Pauses the active copy between chunks and emits `copy-paused` to the frontend.
#[tauri::command]
fn pause_copy(window: WebviewWindow, state: State<AppState>) {
    if let Some(ref pc) = *state.pause_cancel.lock().unwrap() {
        pc.pause();
        let _ = window.emit("copy-paused", ());
    }
}

/// Resumes a paused copy and emits `copy-resumed` to the frontend.
#[tauri::command]
fn resume_copy(window: WebviewWindow, state: State<AppState>) {
    if let Some(ref pc) = *state.pause_cancel.lock().unwrap() {
        pc.resume();
        let _ = window.emit("copy-resumed", ());
    }
}

/// Cancels the active copy operation.
#[tauri::command]
fn cancel_copy(state: State<AppState>) {
    if let Some(ref pc) = *state.pause_cancel.lock().unwrap() {
        pc.cancel();
    }
}

// ── Folder dialog ─────────────────────────────────────────────────────────────
//
// The folder picker is handled entirely in JavaScript:
//   const path = await window.__TAURI__.dialog.open({ directory: true });
//
// WHY? There is a known deadlock in Tauri v2 on Linux: the `tauri_plugin_dialog`
// `pick_folder` callback-based API requires the GTK main thread. But Tauri command
// handlers run on Tokio worker threads. Calling `pick_folder` from a worker thread
// deadlocks because the GTK main thread is busy running the Tauri event loop.
// Delegating the dialog call entirely to JavaScript (which runs on the WebView's
// main thread) sidesteps this issue completely with no loss of functionality.

/// Opens each path in a file manager window using the platform-native launcher.
///
/// Called by JavaScript in the `copy-done` event handler when the "Open destinations"
/// checkbox is checked and the copy completed successfully.
///
/// ### Platform-specific launchers
/// - **Linux**   — `xdg-open`: part of the XDG spec, supported by GNOME, KDE,
///   Cinnamon, XFCE, and virtually every Linux desktop environment.
/// - **macOS**   — `open`: built-in shell command, opens in Finder.
/// - **Windows** — `explorer`: opens a Windows Explorer window at the given path.
///
/// Each path is opened as a **separate child process** with `.spawn()`.
/// `.spawn()` is non-blocking: it starts the child process and returns immediately
/// without waiting for it to exit (unlike `.output()` which waits).
///
/// ### `#[cfg(target_os = "…")]` — conditional compilation
/// This attribute tells the Rust compiler to include the following expression only
/// when building for the specified operating system. The other branches are
/// **completely absent** from the compiled binary — there is no runtime branching.
/// This is determined at compile time, not at runtime.
///
/// ### `let _ = …spawn()`
/// `.spawn()` returns `io::Result<Child>`. We discard it with `let _ = …`
/// because a failure to open the file manager (e.g. `xdg-open` not installed)
/// is non-critical — the copy already succeeded.
/// Sends a native OS notification at the end of a copy operation.
///
/// ### Linux (including Linux Mint / Cinnamon)
/// Uses `notify-send` CLI directly via `std::process::Command`.
/// This always works on Linux Mint which ships libnotify by default.
/// `tauri_plugin_notification` can fail silently on Cinnamon because it
/// tries to contact a D-Bus notification daemon that may not respond
/// correctly in all session configurations.
///
/// ### macOS
/// Uses tauri_plugin_notification → Notification Center.
///
/// ### Windows
/// Uses tauri_plugin_notification → Toast notification (Action Center).
///
/// Errors are silently ignored — a missing notification is non-critical.
/// The result is already visible in the Bartleby UI.
#[tauri::command]
#[allow(unused_variables)]
fn send_notification(app: tauri::AppHandle, title: String, body: String) {
    #[cfg(target_os = "linux")]
    {
        // notify-send is the standard CLI for libnotify on Linux.
        // It is pre-installed on Linux Mint, Ubuntu, Fedora, and most distros.
        // -a "Bartleby" : sets the application name shown in the notification.
        // -i "dialog-information" : uses a standard system icon.
        // Timeout: default (system decides, typically 5 seconds).
        // CREATE_NO_WINDOW is not needed on Linux — no console window issue.
        let _ = std::process::Command::new("notify-send")
            .arg("-a").arg("Bartleby")
            .arg("-i").arg("dialog-information")
            .arg(&title)
            .arg(&body)
            .spawn(); // spawn() is non-blocking — we don't wait for completion
        return;
    }

    // macOS and Windows: use tauri_plugin_notification
    #[cfg(not(target_os = "linux"))]
    {
        #[cfg(not(target_os = "linux"))]
use tauri_plugin_notification::NotificationExt;
        let _ = app.notification()
            .builder()
            .title(title)
            .body(body)
            .show();
    }

}


#[tauri::command]
fn open_destinations(paths: Vec<String>) {
    // Iterate over each destination path string.
    // `for path in paths` moves each String out of the Vec (no cloning needed).
    for path in paths {
        // Linux: use the XDG file opener
        #[cfg(target_os = "linux")]
        let _ = std::process::Command::new("xdg-open").arg(&path).spawn();

        // macOS: use the Finder opener
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg(&path).spawn();

        // Windows: use Windows Explorer
        #[cfg(target_os = "windows")]
        let _ = std::process::Command::new("explorer").arg(&path).spawn();
    }
}

/// Detects whether the operating system is currently using a dark colour scheme.
///
/// Returns `true` for dark mode, `false` for light mode (or if detection fails).
///
/// ### Why not use the CSS `prefers-color-scheme` media query?
/// Inside Tauri's WebKit WebView on Linux, the `@media (prefers-color-scheme: dark)`
/// CSS query is unreliable — the WebView does not always receive GTK theme change
/// signals, so it may report "light" even when the system uses a dark theme.
/// Querying the desktop environment directly from Rust, via `gsettings` or
/// environment variables, is the only reliable approach on Linux.
///
/// ### Linux detection strategy (three fallbacks in priority order)
///
/// 1. **`gsettings get org.gnome.desktop.interface color-scheme`** — the official
///    mechanism for GNOME 42+ (and GTK4 apps). Returns `'prefer-dark'` for dark mode.
///    This is the most authoritative source on modern GNOME and Ubuntu.
///
/// 2. **`GTK_THEME` environment variable** — some desktop environments and launcher
///    scripts set this variable when starting apps. If it contains "dark"
///    (e.g. `Adwaita:dark`), the user intends a dark theme.
///
/// 3. **`gsettings get org.cinnamon.desktop.interface gtk-theme`** — Linux Mint /
///    Cinnamon-specific. Returns the active GTK theme name (e.g. `'Mint-Y-Dark-Aqua'`).
///    If the theme name contains "dark", dark mode is active.
///
/// ### `#[cfg(target_os = "…")]`
/// Only one block is compiled per platform. The Linux block is absent from macOS and
/// Windows binaries, and vice versa. This prevents unused-import warnings and avoids
/// accidental calls to platform-specific APIs.
///
/// ### `String::from_utf8_lossy(&out.stdout)`
/// `Command::output()` returns stdout as `Vec<u8>` (raw bytes).
/// `from_utf8_lossy` converts `&[u8]` to a `Cow<str>` (Copy-on-Write string):
///   - If the bytes are valid UTF-8, it returns a borrowed `&str` (no allocation).
///   - If any byte is invalid UTF-8, it replaces it with the U+FFFD replacement
///     character and returns an owned `String`.
/// We call `.to_lowercase()` to make the comparison case-insensitive.
#[tauri::command]
fn is_system_dark_mode() -> bool {

    // ── Linux ─────────────────────────────────────────────────────────────────
    #[cfg(target_os = "linux")]
    {
        // Strategy 1: GNOME 42+ / GTK4 colour-scheme key
        // `std::process::Command::new("gsettings")` creates a command builder.
        // `.args([…])` appends multiple arguments at once (equivalent to three `.arg()` calls).
        // `.output()` runs the command, waits for it to finish, and captures stdout + stderr.
        // `.ok()?` would propagate None in a function returning Option — but here we use
        // `if let Ok(out)` instead, which silently continues if gsettings is not installed.
        if let Ok(out) = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "color-scheme"])
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout).to_lowercase();
            // Returns `'prefer-dark'` in dark mode, `'default'` in light mode.
            // `contains("dark")` is safe because neither "default" nor "prefer-dark"
            // is a substring of the other… and both contain or don't contain "dark".
            if s.contains("dark") { return true; }
        }

        // Strategy 2: GTK_THEME environment variable
        // `std::env::var("GTK_THEME")` returns `Ok(String)` if set, `Err` if absent.
        // Typical values: `"Adwaita:dark"`, `"Mint-Y-Dark-Aqua"`, `"Yaru-dark"`.
        if let Ok(theme) = std::env::var("GTK_THEME") {
            if theme.to_lowercase().contains("dark") { return true; }
        }

        // Strategy 3: Linux Mint / Cinnamon — gtk-theme key
        // On Cinnamon, the colour scheme is stored as a theme name, not a
        // "prefer-dark" flag. We match the pattern "dark" in the theme name.
        if let Ok(out) = std::process::Command::new("gsettings")
            .args(["get", "org.cinnamon.desktop.interface", "gtk-theme"])
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout).to_lowercase();
            if s.contains("dark") { return true; }
        }

        // None of the strategies indicated dark mode → assume light
        false
    }

    // ── macOS ─────────────────────────────────────────────────────────────────
    #[cfg(target_os = "macos")]
    {
        // `defaults read -g AppleInterfaceStyle` returns "Dark" in dark mode.
        // In light mode the key is absent and the command exits with a non-zero code,
        // returning empty stdout — so `.output()` succeeds but the content is empty.
        // We compare the trimmed, lowercased output to "dark" to handle both cases.
        if let Ok(out) = std::process::Command::new("defaults")
            .args(["read", "-g", "AppleInterfaceStyle"])
            .output()
        {
            // `.trim()` strips leading/trailing whitespace and newlines.
            // `.to_lowercase()` normalises "Dark" → "dark" for comparison.
            return String::from_utf8_lossy(&out.stdout).trim().to_lowercase() == "dark";
        }
        false
    }

    // ── Windows ───────────────────────────────────────────────────────────────
    #[cfg(target_os = "windows")]
    {
        // Windows stores the dark mode preference in the registry at:
        //   HKEY_CURRENT_USER\SOFTWARE\Microsoft\Windows\CurrentVersion\Themes\Personalize
        //   Value: AppsUseLightTheme  REG_DWORD
        //     0 = dark mode active
        //     1 = light mode active (the default)
        //
        // We query this with the `reg` command-line tool, which is always present
        // on Windows — no extra dependency needed.
        //
        // ### Why `reg query` instead of the `winreg` crate?
        // Adding `winreg` to Cargo.toml works but increases compile time and binary
        // size for a single DWORD read. `reg query` is a built-in Windows tool
        // that has been available since Windows XP. The output is predictable and
        // easy to parse. The process creation overhead (~5 ms) is acceptable since
        // this is called only once at startup.
        //
        // ### `CREATE_NO_WINDOW` on Windows
        // We apply the same no-console-window flag used in metadata.rs / pdf_report.rs
        // to prevent a cmd.exe flash when `reg` is spawned at startup.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        if let Ok(out) = std::process::Command::new("reg")
            .args([
                "query",
                "HKCU\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize",
                "/v", "AppsUseLightTheme",
            ])
            .creation_flags(CREATE_NO_WINDOW) // suppress cmd.exe flash
            .output()
        {
            // `reg query` output looks like:
            //   HKEY_CURRENT_USER\SOFTWARE\...\Personalize
            //       AppsUseLightTheme    REG_DWORD    0x0
            //
            // We search for "0x0" in the output:
            //   "0x0" → AppsUseLightTheme = 0 → dark mode ON  → return true
            //   "0x1" → AppsUseLightTheme = 1 → light mode    → return false
            // `.to_lowercase()` handles potential "0X0" variations.
            let s = String::from_utf8_lossy(&out.stdout).to_lowercase();
            // The line containing the value ends with either "0x0" (dark) or "0x1" (light).
            // We look for "applsuselighttheme" and then check if its value is 0x0.
            if s.contains("appsuselighttheme") {
                // Find the value: split on whitespace, look for "0x0" token
                return s.split_whitespace().any(|tok| tok == "0x0");
            }
        }

        // Fallback: try the CSS media query via a known Windows 10/11 approach.
        // If `reg` failed (shouldn't happen on Windows), default to light mode.
        false
    }

    // ── Any other platform (FreeBSD, OpenBSD, etc.) ───────────────────────────
    // `not(any(…))` is true when none of the listed conditions are true.
    // This block ensures the function compiles on platforms not listed above.
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        false
    }
}

/// Applies a light/dark variant to one window's native decorations
/// (title bar and border).
///
/// `mode` is `"dark"` / `"light"` to force a variant (overriding the OS), or
/// any other value (`"default"`) to follow the OS.
///
/// - `win.set_theme()` forces — or, with `None`, releases — the variant and
///   the WebView colour scheme; on Windows alone this is unreliable for the
///   native title bar.
/// - Windows: `DWMWA_USE_IMMERSIVE_DARK_MODE` is applied directly so the
///   title bar always obeys the chosen theme.
/// - Linux: window managers (Muffin, Mutter, KWin) read the X11 property
///   `_GTK_THEME_VARIANT` to pick the decoration variant; it is written
///   directly via `gdk::property_change()` on the GTK main thread.
fn apply_window_decoration_theme(win: &tauri::WebviewWindow, mode: &str) {
    let _ = win.set_theme(match mode {
        "dark"  => Some(Theme::Dark),
        "light" => Some(Theme::Light),
        _       => None, // follow the OS
    });

    #[cfg(target_os = "windows")]
    {
        let dark = match mode {
            "dark"  => true,
            "light" => false,
            _       => is_system_dark_mode(),
        };
        set_titlebar_dark(win, dark);
    }

    #[cfg(target_os = "linux")]
    {
        let dark = match mode {
            "dark"  => true,
            "light" => false,
            _       => is_system_dark_mode(),
        };
        let win2 = win.clone();
        let _ = win.app_handle().run_on_main_thread(move || {
            let theme_atom = gdk::Atom::intern("_GTK_THEME_VARIANT");
            let utf8_atom  = gdk::Atom::intern("UTF8_STRING");
            let variant: &[u8] = if dark { b"dark" } else { b"light" };
            if let Ok(gtk_win) = win2.gtk_window() {
                if let Some(gdk_win) = gtk::prelude::WidgetExt::window(&gtk_win) {
                    gdk::property_change(
                        &gdk_win,
                        &theme_atom,
                        &utf8_atom,
                        8,
                        gdk::PropMode::Replace,
                        gdk::ChangeData::UChars(variant),
                    );
                }
            }
        });
    }
}

/// Windows: force the native title bar to the dark or light immersive variant.
/// This bypasses the OS theme, which is what the user expects when an explicit
/// "Dark"/"Light" theme is chosen.
#[cfg(target_os = "windows")]
fn set_titlebar_dark(win: &tauri::WebviewWindow, dark: bool) {
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
    if let Ok(hwnd) = win.hwnd() {
        let value: i32 = if dark { 1 } else { 0 };
        unsafe {
            DwmSetWindowAttribute(
                hwnd.0 as _,
                DWMWA_USE_IMMERSIVE_DARK_MODE as _,
                &value as *const i32 as *const _,
                std::mem::size_of::<i32>() as u32,
            );
        }
    }
}

/// Sets the light/dark mode for every visible window.
///
/// `theme` is `"dark"` / `"light"` to override the OS, or `"default"` to
/// follow it. Hidden windows are skipped — calling `set_theme()` on a hidden
/// WebView2 window crashes the application on Windows.
///
/// Called by `applyTheme()` in JS whenever the user changes the theme.
#[tauri::command]
fn set_window_theme(app: tauri::AppHandle, theme: String) {
    for label in &["main", "verifier"] {
        if let Some(win) = app.get_webview_window(label) {
            if win.is_visible().unwrap_or(false) {
                apply_window_decoration_theme(&win, &theme);
            }
        }
    }
}

/// Returns the application version string from Cargo.toml.
/// Used by the About modal to display the version without hardcoding it in HTML.
#[tauri::command]
fn get_app_version() -> &'static str {
    VERSION
}

/// Returns the user's home directory as a UTF-8 string, or an empty string if unavailable.
/// Used by JavaScript to shorten displayed paths: /home/user/FOO → ~/FOO.
#[tauri::command]
fn get_home_dir() -> String {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

// ── Volume info ───────────────────────────────────────────────────────────────

#[derive(Clone, Serialize)]
struct VolumeInfo {
    ok:          bool,
    label:       String,
    media_type:  String,
    total_bytes: u64,
    free_bytes:  u64,
}

/// Walks up `path` until an existing ancestor is found (for not-yet-created destinations).
fn nearest_existing(path: &str) -> Option<String> {
    let mut p = std::path::PathBuf::from(path);
    for _ in 0..32 {
        if p.exists() { return Some(p.to_string_lossy().into_owned()); }
        if !p.pop() { break; }
    }
    None
}

#[cfg(unix)]
fn vol_space(path: &str) -> Option<(u64, u64)> {
    use std::ffi::CString;
    let cpath = CString::new(path).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(cpath.as_ptr(), &mut st) } != 0 { return None; }
    Some((
        (st.f_blocks as u64).saturating_mul(st.f_frsize as u64),
        (st.f_bavail as u64).saturating_mul(st.f_frsize as u64),
    ))
}

#[cfg(target_os = "windows")]
fn vol_space(path: &str) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let wide: Vec<u16> = std::ffi::OsStr::new(path).encode_wide().chain(std::iter::once(0)).collect();
    let mut avail: u64 = 0;
    let mut total: u64 = 0;
    let mut _free: u64 = 0;
    if unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut avail, &mut total, &mut _free) } == 0 {
        return None;
    }
    Some((total, avail))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn vol_space(_path: &str) -> Option<(u64, u64)> { None }

#[cfg(target_os = "linux")]
fn lsblk_val(line: &str, field: &str) -> String {
    let key = format!("{}=\"", field);
    if let Some(s) = line.find(&key) {
        let rest = &line[s + key.len()..];
        if let Some(e) = rest.find('"') { return rest[..e].to_string(); }
    }
    String::new()
}

#[cfg(target_os = "linux")]
fn vol_label_type(path: &str) -> (String, String) {
    let Ok(out) = std::process::Command::new("lsblk")
        .args(["-P", "-o", "NAME,MOUNTPOINT,LABEL,ROTA,RM,TRAN,PKNAME"])
        .output()
    else { return (String::new(), "Unknown".to_string()); };

    let text  = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();

    // Build NAME → TRAN map so partition rows can inherit the parent device's transport.
    let mut tran_map = std::collections::HashMap::<String, String>::new();
    for line in &lines {
        let name = lsblk_val(line, "NAME");
        let tran = lsblk_val(line, "TRAN");
        if !name.is_empty() && !tran.is_empty() { tran_map.insert(name, tran); }
    }

    let path_p = std::path::Path::new(path);
    let mut best: usize = 0;
    let mut label = String::new();
    let mut mtype = "Unknown".to_string();

    for line in &lines {
        let mp = lsblk_val(line, "MOUNTPOINT");
        if mp.is_empty() { continue; }
        if !path_p.starts_with(std::path::Path::new(&*mp)) { continue; }
        if mp.len() <= best { continue; }
        best  = mp.len();
        label = lsblk_val(line, "LABEL");
        let rota  = lsblk_val(line, "ROTA");
        let rm    = lsblk_val(line, "RM");
        let name  = lsblk_val(line, "NAME");
        let mut tran = lsblk_val(line, "TRAN").to_lowercase();
        if tran.is_empty() {
            let pk = lsblk_val(line, "PKNAME");
            if !pk.is_empty() {
                tran = tran_map.get(&pk).cloned().unwrap_or_default().to_lowercase();
            }
        }
        mtype = if rota == "1" {
            "HDD".to_string()
        } else if tran == "mmc" || name.starts_with("mmcblk") {
            "SD".to_string()
        } else if tran == "usb" || rm == "1" {
            "Flash".to_string()
        } else {
            "SSD".to_string()
        };
    }
    (label, mtype)
}

#[cfg(target_os = "macos")]
fn vol_label_type(path: &str) -> (String, String) {
    let Ok(out) = std::process::Command::new("diskutil")
        .args(["info", path])
        .output()
    else { return (String::new(), "Unknown".to_string()); };
    let text = String::from_utf8_lossy(&out.stdout);
    let label = text.lines()
        .find(|l| l.contains("Volume Name:"))
        .and_then(|l| l.split(':').nth(1))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let ssd = text.lines().any(|l| l.contains("Solid State:") && l.to_lowercase().contains("yes"));
    let rem = text.lines().any(|l| l.contains("Removable Media:") && l.to_lowercase().contains("removable"));
    let mtype = if rem { "Flash" } else if ssd { "SSD" } else { "HDD" };
    (label, mtype.to_string())
}

/// Returns true if the volume whose root is `root` (null-terminated UTF-16,
/// e.g. `['C',':','\\',0]`) is a solid-state device (no seek penalty).
/// Opens `\\.\X:` and sends IOCTL_STORAGE_QUERY_PROPERTY with
/// StorageDeviceSeekPenaltyProperty (id=7). Falls back to false on any error
/// so the caller can safely default to "HDD".
#[cfg(target_os = "windows")]
fn is_fixed_ssd(root: &[u16]) -> bool {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::IO::{DeviceIoControl, OVERLAPPED};

    // Build "\\.\X:" (device path) from the volume root "X:\".
    // We only handle single-letter drive paths (root starts with "X:").
    let root_end = root.iter().position(|&c| c == 0).unwrap_or(root.len());
    if root_end < 2 { return false; }

    // \\.\  prefix in UTF-16
    let mut dev: Vec<u16> = vec![b'\\' as u16, b'\\' as u16, b'.' as u16, b'\\' as u16];
    dev.extend_from_slice(&root[..2]); // "X:"
    dev.push(0);

    // Open the volume device (no read/write access needed for IOCTL).
    let h = unsafe {
        CreateFileW(
            dev.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            0,
        )
    };
    if h == INVALID_HANDLE_VALUE { return false; }

    // Input: STORAGE_PROPERTY_QUERY { PropertyId=7 (SeekPenalty), QueryType=0 (Standard) }
    #[repr(C)] struct Query   { prop: u32, qtype: u32, extra: u8 }
    // Output: DEVICE_SEEK_PENALTY_DESCRIPTOR
    #[repr(C)] struct Penalty { _ver: u32, _sz: u32, incurs: u8 }

    let q     = Query   { prop: 7, qtype: 0, extra: 0 };
    let mut p = Penalty { _ver: 0, _sz: 0, incurs: 1 }; // default incurs=1 (HDD) as safe fallback
    let mut returned = 0u32;

    let ok = unsafe {
        DeviceIoControl(
            h,
            0x002D_1400u32, // IOCTL_STORAGE_QUERY_PROPERTY
            &q     as *const Query    as *const c_void,
            std::mem::size_of::<Query>() as u32,
            &mut p as *mut   Penalty  as *mut   c_void,
            std::mem::size_of::<Penalty>() as u32,
            &mut returned,
            std::ptr::null_mut::<OVERLAPPED>(),
        )
    };
    unsafe { CloseHandle(h) };
    ok != 0 && p.incurs == 0
}

#[cfg(target_os = "windows")]
fn vol_label_type(path: &str) -> (String, String) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetVolumeInformationW, GetVolumePathNameW,
    };
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_REMOTE: u32 = 4;
    const DRIVE_CDROM: u32 = 5;
    let wide: Vec<u16> = std::ffi::OsStr::new(path).encode_wide().chain(std::iter::once(0)).collect();
    let mut root = vec![0u16; 260];
    if unsafe { GetVolumePathNameW(wide.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return (String::new(), "Unknown".to_string());
    }
    let mtype = match unsafe { GetDriveTypeW(root.as_ptr()) } {
        DRIVE_REMOVABLE => "Flash",
        DRIVE_FIXED     => if is_fixed_ssd(&root) { "SSD" } else { "HDD" },
        DRIVE_REMOTE    => "Network",
        DRIVE_CDROM     => "CD-ROM",
        _               => "Unknown",
    };
    let mut lbuf = vec![0u16; 260];
    unsafe {
        GetVolumeInformationW(
            root.as_ptr(), lbuf.as_mut_ptr(), lbuf.len() as u32,
            std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(),
            std::ptr::null_mut(), 0,
        );
    }
    let lend  = lbuf.iter().position(|&c| c == 0).unwrap_or(lbuf.len());
    let label = String::from_utf16_lossy(&lbuf[..lend]);
    (label, mtype.to_string())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn vol_label_type(_path: &str) -> (String, String) {
    (String::new(), "Unknown".to_string())
}

#[tauri::command]
fn get_volume_info(path: String) -> VolumeInfo {
    let none = VolumeInfo { ok: false, label: String::new(), media_type: String::new(), total_bytes: 0, free_bytes: 0 };
    let probe = match nearest_existing(&path) { Some(p) => p, None => return none };
    let (total, free) = match vol_space(&probe) { Some(v) => v, None => return none };
    let (label, media_type) = vol_label_type(&probe);
    VolumeInfo { ok: true, label, media_type, total_bytes: total, free_bytes: free }
}

/// Saves the in-app copy log to a timestamped `.txt` file in the logs directory.
///
/// Files are written to `{config_dir}/bartleby/logs/YYYY-MM-DD_HH-MM-SS.txt`:
///   Linux:   `~/.config/bartleby/logs/`
///   macOS:   `~/Library/Application Support/bartleby/logs/`
///   Windows: `%APPDATA%\bartleby\logs\`
///
/// The directory is created automatically if it does not yet exist.
/// Returns the absolute path of the saved file so the JS side can display it.
#[tauri::command]
fn save_log(content: String) -> Result<String, String> {
    let config_dir = dirs::config_dir()
        .ok_or_else(|| "Cannot determine config directory".to_string())?;
    let logs_dir = config_dir.join("bartleby").join("logs");
    std::fs::create_dir_all(&logs_dir)
        .map_err(|e| format!("Cannot create logs directory: {}", e))?;
    let filename = chrono::Local::now()
        .format("%Y-%m-%d_%H-%M-%S.txt")
        .to_string();
    let path = logs_dir.join(&filename);
    std::fs::write(&path, &content)
        .map_err(|e| format!("Cannot write log file: {}", e))?;
    Ok(path.to_string_lossy().to_string())
}

// ── Verifier window commands ──────────────────────────────────────────────────

/// Show (or focus) the verification tool window.
///
/// The window itself is created once, hidden, during `setup()` — see the
/// builder chain in `main()`. Here we only reveal it, which is reliable on
/// every platform (runtime WebView2 window creation deadlocks on Windows).
///
/// The native window theme is applied *after* `show()`: calling `set_theme()`
/// on a hidden WebView2 window crashes the application on Windows.
#[tauri::command]
fn open_verifier_window(app: tauri::AppHandle, state: State<AppState>) -> Result<(), String> {
    let win = app
        .get_webview_window("verifier")
        .ok_or_else(|| "Verification window is not available".to_string())?;
    win.show().map_err(|e| e.to_string())?;
    let _ = win.unminimize();
    win.set_focus().map_err(|e| e.to_string())?;

    let mode = state.settings.lock().unwrap().theme.clone();
    apply_window_decoration_theme(&win, &mode);

    // Ask verifier.js to refresh its skin / colour scheme — the theme may have
    // changed in the main window while this window was hidden.
    let _ = win.emit("verifier-shown", ());
    Ok(())
}

/// Parse a checksum or MHL file and return the file list without hashing.
/// Called when a file is loaded so the table populates immediately.
#[tauri::command]
fn parse_verification_file(file_path: String) -> Result<verify_engine::FileListResult, String> {
    verify_engine::parse_file(std::path::PathBuf::from(file_path))
        .map_err(|e| e.to_string())
}

/// Start verifying a checksum or MHL file in a background thread.
/// Progress and results are emitted as `"verify-progress"` / `"verify-done"` /
/// `"verify-error"` events on the verifier window.
#[tauri::command]
fn start_verification(window: tauri::WebviewWindow, state: State<AppState>, file_path: String) {
    let pc = copy_engine::PauseCancel::new();
    *state.verify_pc.lock().unwrap() = Some(pc.clone());
    verify_engine::run(std::path::PathBuf::from(file_path), window, pc);
}

#[tauri::command]
fn pause_verification(window: tauri::WebviewWindow, state: State<AppState>) {
    if let Some(ref pc) = *state.verify_pc.lock().unwrap() {
        pc.pause();
        let _ = window.emit("verify-paused", ());
    }
}

#[tauri::command]
fn resume_verification(window: tauri::WebviewWindow, state: State<AppState>) {
    if let Some(ref pc) = *state.verify_pc.lock().unwrap() {
        pc.resume();
        let _ = window.emit("verify-resumed", ());
    }
}

#[tauri::command]
fn cancel_verification(state: State<AppState>) {
    if let Some(ref pc) = *state.verify_pc.lock().unwrap() {
        pc.cancel();
    }
}

/// Save a verification result as a self-contained HTML report.
#[tauri::command]
fn save_verify_html(
    result:      verify_engine::VerifyResult,
    output_path: String,
) -> Result<(), String> {
    verify_engine::write_html_report(&result, std::path::Path::new(&output_path))
        .map_err(|e| e.to_string())
}

/// Generate a post-verification MHL (`<process>verify</process>`, generation N+1)
/// for the original MHL that was just verified.
#[tauri::command]
fn generate_post_verify_mhl(
    verified_mhl: String,
    result:       verify_engine::VerifyResult,
    state:        tauri::State<AppState>,
) -> Result<String, String> {
    let settings = state.settings.lock().unwrap().clone();
    verify_engine::write_post_verify_mhl(
        std::path::Path::new(&verified_mhl),
        &result,
        &settings,
    )
    .map(|p| p.display().to_string())
    .map_err(|e| e.to_string())
}

// ── Linux: GTK theme bootstrap ────────────────────────────────────────────────

/// Reads the saved theme/skin from settings.json and sets GTK_THEME so the
/// native window border matches the app theme from the very first frame.
///
/// GTK_THEME is consumed by gtk_init(), which Tauri calls inside
/// tauri::Builder::default(). This function must therefore run before that call.
/// Setting GTK_THEME after Tauri is already initialised has no effect on the
/// window border.
///
/// Any failure (file absent on first launch, parse error, missing HOME) is
/// silently ignored so Bartleby always starts normally.
#[cfg(target_os = "linux")]
fn apply_gtk_theme_from_settings() {
    let settings_path = match dirs::config_dir() {
        Some(d) => d.join("bartleby").join("settings.json"),
        None    => return,
    };

    let content = match std::fs::read_to_string(&settings_path) {
        Ok(c)  => c,
        Err(_) => return,
    };

    #[derive(serde::Deserialize)]
    struct ThemeHint {
        #[serde(default)]
        theme: String,
        #[serde(default)]
        skin: String,
    }

    let hint: ThemeHint = match serde_json::from_str(&content) {
        Ok(h)  => h,
        Err(_) => return,
    };

    let gtk_theme = resolve_gtk_theme(&hint.theme, &hint.skin);
    if !gtk_theme.is_empty() {
        // Called before tauri::Builder::default() spawns any threads, so
        // set_var is safe here despite being documented as unsafe in
        // multithreaded contexts.
        std::env::set_var("GTK_THEME", gtk_theme);
    }
}

/// Maps (skin, theme) to a GTK theme name string.
///
/// Returns "" for theme="default" — in that case GTK_THEME is not set and
/// the OS decides the window border theme, which is exactly what "Follow system"
/// should do.
///
/// macOS and Windows skins have no native GTK equivalent; Adwaita is used as
/// the most neutral fallback available on all GTK3 systems.
#[cfg(target_os = "linux")]
fn resolve_gtk_theme(theme: &str, skin: &str) -> &'static str {
    match (skin, theme) {
        ("mint-y-aqua", "dark")  => "Mint-Y-Dark-Aqua",
        ("mint-y-aqua", "light") => "Mint-Y-Aqua",
        ("adwaita",     "dark")  => "Adwaita-dark",
        ("adwaita",     "light") => "Adwaita",
        ("macos",       "dark")  => "Adwaita-dark",
        ("macos",       "light") => "Adwaita",
        ("windows11",   "dark")  => "Adwaita-dark",
        ("windows11",   "light") => "Adwaita",
        _                        => "",
    }
}
