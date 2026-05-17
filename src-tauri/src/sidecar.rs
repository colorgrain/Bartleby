use std::process::Command;

/// Looks for `bundled_name` next to the running executable (the Tauri sidecar
/// location). Falls back to `fallback_name` resolved via PATH if absent.
///
/// On macOS, the PATH fallback injects Homebrew directories because GUI apps
/// launched from Finder/Dock inherit a minimal PATH that excludes them.
fn sidecar_or_system(bundled_name: &str, fallback_name: &str) -> Command {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = if cfg!(windows) {
                dir.join(format!("{}.exe", bundled_name))
            } else {
                dir.join(bundled_name)
            };
            if candidate.exists() {
                return Command::new(candidate);
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        let mut cmd = Command::new(fallback_name);
        cmd.env("PATH", "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin");
        return cmd;
    }
    Command::new(fallback_name)
}

/// Returns a `Command` for ffmpeg, preferring the bundled `bartleby-ffmpeg` binary.
pub fn ffmpeg_cmd() -> Command {
    sidecar_or_system("bartleby-ffmpeg", "ffmpeg")
}

/// Returns a `Command` for mediainfo, preferring the bundled `bartleby-mediainfo` binary.
pub fn mediainfo_cmd() -> Command {
    sidecar_or_system("bartleby-mediainfo", "mediainfo")
}
