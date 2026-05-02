// build.rs — Bartleby Tauri build script
//
// ══════════════════════════════════════════════════════════════════════════════
// WHAT IS build.rs?
// ══════════════════════════════════════════════════════════════════════════════
//
// `build.rs` is a special Rust source file that Cargo compiles and **executes
// on the build machine** before compiling the rest of the crate.
//
// Cargo looks for it automatically in the crate root (next to Cargo.toml).
// Its compiled binary runs at the start of every `cargo build` invocation.
// It runs on the developer's machine (or CI server) — never on the end user's machine.
//
// Common uses of build scripts:
//   • Generating Rust source code at build time (e.g. protobuf bindings).
//   • Compiling C/C++ code and linking it into the Rust binary (via the `cc` crate).
//   • Detecting platform capabilities (OS version, available libraries).
//   • Embedding version numbers, git hashes, or other metadata.
//   • Configuring linker flags via `println!("cargo:rustc-link-lib=…")`.
//
// ── Tauri's specific use of build.rs ─────────────────────────────────────────
//
// For Tauri, `tauri_build::build()` performs several critical compile-time tasks:
//
//   1. ICON EMBEDDING
//      Reads the icon files listed in tauri.conf.json → bundle.icon and embeds
//      them into the binary. On Windows this produces a .ico resource in the .exe.
//      On macOS it produces a .icns. On Linux the icons are used for .deb/.AppImage.
//
//   2. MANIFEST GENERATION (Windows only)
//      Creates a Windows application manifest (.manifest file) that declares:
//        • DPI awareness (so the window is sharp on high-DPI / 4K displays).
//        • Requested execution level (typically "asInvoker" — no UAC elevation).
//        • Compatible Windows version declarations (Win7+, Win8+, Win10+, Win11).
//      Without this manifest, Windows may apply legacy DPI scaling that makes
//      the WebView blurry on HiDPI screens.
//
//   3. TAURI CONFIGURATION VALIDATION
//      Reads and validates tauri.conf.json at compile time. If the config has
//      syntax errors or invalid values, the build fails with a clear error message
//      rather than crashing at runtime.
//
//   4. CAPABILITIES RESOLUTION
//      In Tauri v2, security permissions are declared in capabilities/*.json files.
//      `tauri_build::build()` validates these capability files and links them to
//      the compiled binary. Invalid permission declarations cause a compile error.
//
//   5. ASSET BUNDLING CONFIGURATION
//      Sets up Cargo configuration for embedding the frontend assets (the src/
//      directory containing index.html, main.js, style.css) into the binary.
//      This is why the Tauri app works without a running web server.
//
// ── Why is this only one line? ────────────────────────────────────────────────
//
// All the complexity is inside the `tauri_build` crate (a build-time dependency
// declared in Cargo.toml under [build-dependencies]). Our build.rs just calls
// into it. This is the standard pattern for Tauri: the crate does all the work;
// our script is a thin entry point.
//
// ── build-dependencies vs dependencies ────────────────────────────────────────
//
// Cargo distinguishes three types of dependencies:
//
//   [dependencies]       — compiled into the final binary, run on the user's machine.
//   [build-dependencies] — compiled into build.rs only, run on the build machine.
//   [dev-dependencies]   — only used for `cargo test` and `cargo bench`.
//
// `tauri-build` is in [build-dependencies] because it only needs to run during
// the build process. Including it in [dependencies] would bloat the final binary
// with code that is never executed by the app itself.
//
// ── What happens if build.rs is absent? ──────────────────────────────────────
//
// Cargo simply skips the build script phase. For Tauri, this means:
//   • No icon embedding → the binary has no app icon.
//   • No Windows manifest → DPI scaling issues on Windows HiDPI displays.
//   • No capabilities validation → invalid permissions are only caught at runtime.
//   • The build may fail because tauri_build sets linker flags that are required.
//
// ══════════════════════════════════════════════════════════════════════════════

fn main() {
    // `tauri_build::build()` is the entry point of the tauri-build crate.
    //
    // It returns `()` on success. On failure (e.g. tauri.conf.json not found,
    // invalid configuration, icon files missing) it calls `panic!()` with a
    // descriptive error message, which causes `cargo build` to fail visibly.
    //
    // The function internally uses `println!("cargo:…")` instructions to
    // communicate with Cargo. These are standard Cargo build script directives:
    //   cargo:rustc-link-lib=…    → link a native library
    //   cargo:rerun-if-changed=…  → re-run build.rs only if a file changes
    //   cargo:rustc-env=…         → set an environment variable for the Rust compiler
    //   cargo:rustc-cfg=…         → enable a conditional compilation flag
    //
    // Tauri uses `cargo:rerun-if-changed=tauri.conf.json` (and similar) so that
    // `cargo build` only reruns this script when the Tauri configuration changes —
    // not on every incremental rebuild. This keeps build times fast.
    tauri_build::build()
}
