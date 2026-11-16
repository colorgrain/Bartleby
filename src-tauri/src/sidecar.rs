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

/// Returns the desktop icon for a MIME type (64×64 RGBA), looked up through the
/// GTK icon theme by a small `python3` + PyGObject script. Linux only: on macOS
/// `/usr/bin/python3` can pop up the "install developer tools" dialog, and
/// Windows has no GTK — callers fall back to their own placeholder there.
///
/// Security: the MIME type and output path are passed as `sys.argv`, never
/// interpolated into the script, so a crafted file extension cannot inject code.
/// Results are cached per MIME type, so a job with thousands of files of the
/// same type spawns python at most once per type.
pub fn mime_icon(mime: &str) -> Option<image::RgbaImage> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Option<image::RgbaImage>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(hit) = cache.lock().ok().and_then(|c| c.get(mime).cloned()) {
        return hit;
    }
    let icon = lookup_mime_icon(mime);
    if let Ok(mut c) = cache.lock() { c.insert(mime.to_string(), icon.clone()); }
    icon
}

#[cfg(target_os = "linux")]
fn lookup_mime_icon(mime: &str) -> Option<image::RgbaImage> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static SEQ: AtomicUsize = AtomicUsize::new(0);
    const SCRIPT: &str = r#"import gi, sys
gi.require_version('Gtk', '3.0')
from gi.repository import Gio, Gtk, GdkPixbuf
ct = Gio.content_type_from_mime_type(sys.argv[1])
if not ct: sys.exit(1)
icon = Gio.content_type_get_icon(ct)
theme = Gtk.IconTheme.get_default()
info = theme.lookup_by_gicon(icon, 64, 0)
if not info: sys.exit(1)
pb = info.load_icon()
if not pb: sys.exit(1)
pb = pb.scale_simple(64, 64, GdkPixbuf.InterpType.BILINEAR)
pb.savev(sys.argv[2], 'png', [], [])
"#;
    let tmp = std::env::temp_dir().join(format!(
        "_bartleby_icon_{}_{}.png", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_file(&tmp);
    let ok = Command::new("python3")
        .arg("-c").arg(SCRIPT).arg(mime).arg(&tmp)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let img = if ok { image::open(&tmp).ok().map(|i| i.into_rgba8()) } else { None };
    let _ = std::fs::remove_file(&tmp);
    img
}

#[cfg(not(target_os = "linux"))]
fn lookup_mime_icon(_mime: &str) -> Option<image::RgbaImage> {
    None
}

#[cfg(test)]
mod tests {
    // S1: a crafted MIME / extension string must never be executed as Python.
    #[test]
    fn mime_icon_does_not_execute_crafted_input() {
        let marker = std::env::temp_dir().join(format!("bartleby_pwned_{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let evil = format!(
            "x')\nopen('{}', 'w').write('x')\n#", marker.display());
        let _ = super::mime_icon(&evil);
        let evil2 = format!(
            "x'+str(open('{}', 'w').write('x'))+'", marker.display());
        let _ = super::mime_icon(&evil2);
        assert!(!marker.exists(), "crafted input was executed");
    }
}
