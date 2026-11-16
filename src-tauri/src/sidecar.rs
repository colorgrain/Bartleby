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

/// Extracts one video frame, scaled and padded to 224×144, into the JPEG `out`.
///
/// `pad_color` is the ffmpeg colour of the letterbox bars ("black" for the PDF,
/// "black@0" = transparent for the HTML report); `pix_fmt` the output pixel
/// format ("rgb24" / "rgba"). Returns `Err(last stderr line)` when every
/// attempt failed.
///
/// ## Fallbacks (only used when the normal command fails)
/// 1. **Invalid colour tags.** Since FFmpeg 9, the scaler refuses frames whose
///    colour metadata is invalid instead of ignoring it. ARRI ALEXA 35 ProRes
///    MXF files are decoded with "reserved" primaries / transfer (ARRI private
///    labels), so the frame is re-tagged BT.709 before scaling. The tags only
///    drive the YUV→RGB matrix of a 224 px preview; the media is not touched.
/// 2. **Clips shorter than 1 s.** The frame is taken at 1 s to skip black
///    leaders; a shorter clip (e.g. a 24-frame Canon clip) has no frame there,
///    so the first frame is used instead.
pub fn extract_video_thumbnail(
    path:      &std::path::Path,
    out:       &std::path::Path,
    pad_color: &str,
    pix_fmt:   &str,
) -> Result<(), String> {
    let fit = format!(
        "scale=224:144:force_original_aspect_ratio=decrease,\
         pad=224:144:(ow-iw)/2:(oh-ih)/2:color={}", pad_color);
    let retag = format!(
        "setparams=colorspace=bt709:color_primaries=bt709:color_trc=bt709,{}", fit);

    let attempts: [(bool, &str); 4] = [
        (true,  &fit),   // normal: frame at 1 s
        (true,  &retag), // invalid colour tags
        (false, &fit),   // clip shorter than 1 s
        (false, &retag), // both
    ];
    let mut last_err = String::new();
    for (seek, vf) in attempts {
        let mut cmd = ffmpeg_cmd();
        cmd.arg("-y").arg("-hide_banner").arg("-loglevel").arg("error");
        if seek { cmd.arg("-ss").arg("00:00:01"); }
        cmd.arg("-i").arg(path)
            .arg("-map").arg("0:v:0")
            .arg("-vframes").arg("1")
            .arg("-vf").arg(vf)
            .arg("-pix_fmt").arg(pix_fmt)
            .arg("-q:v").arg("3")
            .arg(out);
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let output = cmd.output().map_err(|e| e.to_string())?; // ffmpeg missing: stop
        let written = std::fs::metadata(out).map(|m| m.len() > 0).unwrap_or(false);
        if output.status.success() && written {
            return Ok(());
        }
        last_err = String::from_utf8_lossy(&output.stderr)
            .lines().last().unwrap_or("(empty)").to_string();
        let _ = std::fs::remove_file(out);
    }
    Err(last_err)
}
