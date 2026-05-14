use std::process::Command;

/// Returns a `Command` for `name`, preferring the Tauri-bundled sidecar binary.
///
/// ## How sidecar resolution works
///
/// When Tauri bundles an `externalBin`, it copies the platform-specific binary
/// (e.g. `binaries/ffmpeg-x86_64-unknown-linux-gnu`) next to the main executable
/// stripping the target-triple suffix. At runtime the file is simply `ffmpeg`
/// (or `ffmpeg.exe` on Windows) in the same directory as the app binary.
///
/// This function checks for that file first. If absent (e.g. during development
/// with `cargo run` / `npm run dev`, where no sidecar has been copied), it falls
/// back to searching the system PATH.
///
/// ## macOS PATH extension
///
/// GUI apps launched from Finder / Dock / Spotlight inherit a minimal PATH
/// (`/usr/bin:/bin:/usr/sbin:/sbin`) — Homebrew's directories
/// (`/opt/homebrew/bin` on Apple Silicon, `/usr/local/bin` on Intel) are absent.
/// The fallback path injects them so system-installed binaries are still found
/// when no sidecar is bundled.
pub fn sidecar_cmd(name: &str) -> Command {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = if cfg!(windows) {
                dir.join(format!("{}.exe", name))
            } else {
                dir.join(name)
            };
            if candidate.exists() {
                return Command::new(candidate);
            }
        }
    }

    // Fallback: resolve via PATH
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new(name);
        cmd.env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin");
        return cmd;
    }
    Command::new(name)
}
