//! # Module `html_report`
//!
//! Generates a self-contained HTML report with per-file thumbnails and a metadata table.
//! The output is a single `.html` file with all CSS and images inlined — no external
//! dependencies. It can be opened in any browser and printed to A4 landscape.
//!
//! Thumbnail strategy (same as `pdf_report`):
//! | File type          | Strategy                                                    |
//! |--------------------|-------------------------------------------------------------|
//! | Image (JPG, PNG…)  | `image::open()` → RGBA PNG (transparent background)        |
//! | Video (MP4, MXF…)  | `ffmpeg -ss 1s` extracts one frame as JPEG → PNG           |
//! | Audio (WAV, FLAC…) | `ffmpeg showwavespic` renders a waveform PNG               |
//! | Other (PDF, DOCX…) | `python3 + gi` retrieves the OS MIME icon (64 px)          |
//! | Fallback           | Coloured `<div>` (no image element)                        |
//!
//! Unlike the PDF report, thumbnails preserve alpha (transparent PNG). Browsers
//! composite them against the table row background naturally.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use ::image::imageops::FilterType;
use ::image::RgbaImage;
use chrono::Local;

use crate::metadata::FileMeta;
use crate::settings::Settings;

// ── File type classification (mirrors pdf_report) ────────────────────────────

const IMAGE_EXTS: &[&str] = &[
    "jpg", "jpeg", "png", "tiff", "tif", "webp", "bmp", "gif",
    "heic", "heif", "cr2", "cr3", "nef", "arw", "dng",
];

const VIDEO_EXTS: &[&str] = &[
    "mp4", "mov", "mxf", "avi", "mkv", "m4v", "wmv", "flv", "webm",
    "m2ts", "mts", "ts", "r3d", "braw", "mpg", "mpeg",
];

const AUDIO_EXTS: &[&str] = &[
    "mp3", "wav", "aac", "flac", "ogg", "m4a", "aif", "aiff",
    "opus", "wma", "alac",
];

// ── Helpers ───────────────────────────────────────────────────────────────────

fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x08000000);
    }
    cmd
}

fn ffmpeg_cmd() -> Command {
    crate::sidecar::ffmpeg_cmd()
}

fn he(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
     .replace('"', "&quot;")
}

fn ext_to_mime(ext: &str) -> &str {
    match ext {
        "pdf"          => "application/pdf",
        "doc"          => "application/msword",
        "docx"         => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls"          => "application/vnd.ms-excel",
        "xlsx"         => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt"          => "application/vnd.ms-powerpoint",
        "pptx"         => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "zip"          => "application/zip",
        "tar"          => "application/x-tar",
        "gz"           => "application/gzip",
        "rar"          => "application/x-rar-compressed",
        "7z"           => "application/x-7z-compressed",
        "txt" | "md"   => "text/plain",
        "xml"          => "application/xml",
        "json"         => "application/json",
        "html" | "htm" => "text/html",
        "svg"          => "image/svg+xml",
        "py"           => "text/x-python",
        "rs"           => "text/x-rust",
        _              => "application/octet-stream",
    }
}

// ── Thumbnail generation ──────────────────────────────────────────────────────

fn rgba_to_png_uri(img: RgbaImage) -> Option<String> {
    let mut buf: Vec<u8> = Vec::new();
    ::image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut buf), ::image::ImageFormat::Png)
        .ok()?;
    use base64::Engine as _;
    Some(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&buf)
    ))
}

fn image_thumb_uri(path: &Path) -> Option<String> {
    let rgba = ::image::open(path).ok()?
        .resize(224, 144, FilterType::Triangle)
        .into_rgba8();
    rgba_to_png_uri(rgba)
}

fn video_thumb_uri(path: &Path) -> Option<String> {
    let tmp = std::env::temp_dir().join("_bartleby_html_vthumb.jpg");
    let mut cmd = ffmpeg_cmd();
    cmd.arg("-y")
        .arg("-ss").arg("00:00:01")
        .arg("-i").arg(path)
        .arg("-map").arg("0:v:0")
        .arg("-vframes").arg("1")
        .arg("-vf").arg(
            "scale=224:144:force_original_aspect_ratio=decrease,\
             pad=224:144:(ow-iw)/2:(oh-ih)/2:color=black@0"
        )
        .arg("-pix_fmt").arg("rgba")
        .arg("-q:v").arg("3")
        .arg(&tmp);
    let out = no_window(&mut cmd).output().ok()?;
    if !out.status.success() { return None; }
    let rgba = ::image::open(&tmp).ok().map(|i| i.into_rgba8())?;
    let _ = std::fs::remove_file(&tmp);
    rgba_to_png_uri(rgba)
}

fn audio_wave_uri(path: &Path) -> Option<String> {
    let tmp = std::env::temp_dir().join("_bartleby_html_wave.png");
    let mut cmd = ffmpeg_cmd();
    cmd.arg("-y")
        .arg("-i").arg(path)
        .arg("-filter_complex")
        .arg("showwavespic=s=224x144:colors=#4dffd8|#2a6abf:scale=sqrt")
        .arg("-frames:v").arg("1")
        .arg(&tmp)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let status = no_window(&mut cmd).status().ok()?;
    if !status.success() { return None; }
    let rgba = ::image::open(&tmp).ok().map(|i| i.into_rgba8())?;
    let _ = std::fs::remove_file(&tmp);
    rgba_to_png_uri(rgba)
}

fn mime_icon_uri(ext: &str) -> Option<String> {
    let mime = ext_to_mime(ext);
    let tmp  = std::env::temp_dir().join(format!("_bartleby_html_icon_{}.png", ext));
    let script = format!(
        r#"import gi, sys
gi.require_version('Gtk', '3.0')
from gi.repository import Gio, Gtk, GdkPixbuf
ct = Gio.content_type_from_mime_type('{mime}')
if not ct: sys.exit(1)
icon = Gio.content_type_get_icon(ct)
theme = Gtk.IconTheme.get_default()
info = theme.lookup_by_gicon(icon, 64, 0)
if not info: sys.exit(1)
pb = info.load_icon()
if not pb: sys.exit(1)
pb = pb.scale_simple(64, 64, GdkPixbuf.InterpType.BILINEAR)
pb.savev('{path}', 'png', [], [])
"#,
        mime = mime,
        path = tmp.to_str().unwrap_or(""),
    );
    let mut cmd = Command::new("python3");
    cmd.arg("-c").arg(&script);
    let out = no_window(&mut cmd).output().ok()?;
    if out.status.success() && tmp.exists() {
        let rgba = ::image::open(&tmp).ok().map(|i| i.into_rgba8())?;
        let _ = std::fs::remove_file(&tmp);
        return rgba_to_png_uri(rgba);
    }
    None
}

fn thumb_data_uri(path: &Path, ext: &str) -> Option<String> {
    if IMAGE_EXTS.contains(&ext) {
        if let Some(uri) = image_thumb_uri(path) { return Some(uri); }
    }
    if VIDEO_EXTS.contains(&ext) {
        if let Some(uri) = video_thumb_uri(path) { return Some(uri); }
    }
    if AUDIO_EXTS.contains(&ext) {
        if let Some(uri) = audio_wave_uri(path) { return Some(uri); }
    }
    mime_icon_uri(ext)
}

// Fallback coloured square when no thumbnail is available.
fn fallback_color(ext: &str) -> &'static str {
    match ext {
        "pdf"                               => "#cc1a1a",
        "doc"  | "docx"                     => "#2d6bb5",
        "xls"  | "xlsx"                     => "#217346",
        "ppt"  | "pptx"                     => "#d24726",
        "zip"  | "tar" | "gz" | "rar" | "7z" => "#8c6b22",
        "txt"  | "md"                       => "#7f7f8c",
        _                                   => "#606070",
    }
}

// ── Logo helper ───────────────────────────────────────────────────────────────

fn logo_data_uri(path: &str) -> Option<String> {
    if path.is_empty() { return None; }
    let p = Path::new(path);
    let rgba = ::image::open(p).ok()?.into_rgba8();
    rgba_to_png_uri(rgba)
}

// ── Hex colour parser ─────────────────────────────────────────────────────────

fn hex_to_rgb(hex: &str) -> (u8, u8, u8) {
    let h = hex.trim_start_matches('#');
    if h.len() != 6 { return (31, 158, 222); }
    let r = u8::from_str_radix(&h[0..2], 16).unwrap_or(31);
    let g = u8::from_str_radix(&h[2..4], 16).unwrap_or(158);
    let b = u8::from_str_radix(&h[4..6], 16).unwrap_or(222);
    (r, g, b)
}

// ── Sort key helper ───────────────────────────────────────────────────────────

fn html_sort_key(rel: &str) -> (String, String) {
    let p = Path::new(rel);
    let dir = p.parent()
        .map(|parent| parent.to_string_lossy().replace('\\', "/").to_lowercase())
        .unwrap_or_default();
    let name = p.file_name()
        .map(|n| n.to_string_lossy().to_string().to_lowercase())
        .unwrap_or_default();
    (dir, name)
}

/// Allows only safe inline formatting tags; converts block elements to `<br>`.
/// Prevents any XSS / script injection in the generated HTML report.
fn sanitize_comment(html: &str) -> String {
    let mut result = String::new();
    let mut chars = html.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '<' { result.push(ch); continue; }
        let mut closing = false;
        if chars.peek() == Some(&'/') { chars.next(); closing = true; }
        let mut tag = String::new();
        while let Some(&c) = chars.peek() {
            if c == '>' || c == ' ' || c == '\t' || c == '\n' || c == '/' { break; }
            tag.push(c); chars.next();
        }
        while let Some(c) = chars.next() { if c == '>' { break; } }
        match tag.to_lowercase().as_str() {
            "b" | "strong" => result.push_str(if closing { "</b>" } else { "<b>" }),
            "i" | "em"     => result.push_str(if closing { "</i>" } else { "<i>" }),
            "u"            => result.push_str(if closing { "</u>" } else { "<u>" }),
            "br" | "div" | "p" | "li" => result.push_str("<br>"),
            _              => {} // strip all other tags
        }
    }
    // Collapse consecutive <br> tags, then trim leading/trailing
    while result.contains("<br><br>") { result = result.replace("<br><br>", "<br>"); }
    let s = result.trim_start_matches("<br>").trim_end_matches("<br>").trim();
    s.to_string()
}

// ── Public entry point ────────────────────────────────────────────────────────

pub fn write_html(
    dst_dir:         &Path,
    report_name:     &str,
    timestamp:       &str,
    src_path:        &Path,
    src_total_bytes: u64,
    destinations:    &[PathBuf],
    entries:         &[(FileMeta, String, String, Option<bool>)],
    settings:        &Settings,
    hash_col:        &str,
    comment:         &str,
    location:        &str,
) -> io::Result<()> {
    let out_path = if timestamp.is_empty() {
        dst_dir.join(format!("{}.html", report_name))
    } else {
        dst_dir.join(format!("{}_{}.html", report_name, timestamp))
    };
    let mut out  = std::fs::File::create(&out_path)?;

    let now = Local::now().format("%Y-%m-%d at %I:%M %p").to_string();

    let (r1, g1, b1) = hex_to_rgb(&settings.accent_color_1);
    let a1 = format!("rgb({},{},{})", r1, g1, b1);

    let has_verify = entries.iter().any(|(_, _, _, ok)| ok.is_some());
    let has_hash   = !hash_col.is_empty();

    // ── Inline CSS ────────────────────────────────────────────────────────────
    let css = format!(r#"
*{{box-sizing:border-box;margin:0;padding:0}}
body{{font-family:Arial,Helvetica,sans-serif;font-size:10px;color:#222;background:#fff}}
#header{{padding:10px 16px 10px}}
#header-company-block{{margin-bottom:8px}}
#header-logo{{margin-bottom:3px}}
#header-logo img{{max-height:36px;max-width:110px;object-fit:contain;display:block}}
#header-company{{font-size:13px;font-weight:bold;color:#111;text-transform:uppercase;margin-bottom:1px}}
#header-contact{{font-size:9px;color:#555}}
#header-center{{text-align:center;margin-top:6px}}
#header-center h1{{font-size:16px;font-weight:bold;color:#000;text-transform:uppercase;display:inline-block;border-bottom:2px solid #000;padding-bottom:1px;margin-bottom:4px}}
#header-center .report-line{{font-size:10px;font-weight:bold;color:#333;margin-top:3px}}
#header-center .src-line{{font-size:9px;color:#555;margin-top:2px}}
#header-center .dst-line{{font-size:9px;color:#666;margin-top:1px}}
.rule{{height:2px;background:{a1};margin:0}}
table{{width:100%;border-collapse:collapse;margin-top:0}}
th{{background:{a1};color:#fff;padding:4px 5px;text-align:left;font-size:9px;font-weight:bold;white-space:nowrap}}
td{{padding:3px 5px;vertical-align:middle;border-bottom:1px solid #e0e0e0;font-size:9px}}
tr.row-even td{{background:#f4f4f6}}
tr.row-odd  td{{background:#ffffff}}
tr.dir-row  td{{background:#e8f0f6;border-bottom:1px solid #c8d8e8}}
td.dir-name{{font-size:10px;font-weight:bold;color:#333;padding-left:8px}}
td.thumb{{width:80px;padding:2px;text-align:center;vertical-align:middle}}
td.thumb img{{max-width:76px;max-height:48px;object-fit:contain;display:block;margin:auto}}
td.thumb .fallback{{width:60px;height:38px;border-radius:3px;margin:auto}}
td.status{{width:24px;text-align:center;font-size:11px}}
td.hash{{font-family:monospace;font-size:7.5px;word-break:break-all;max-width:140px}}
td.status.ok{{color:#2a8a3e}} td.status.fail{{color:#cc2200}}
#footer{{margin-top:6px;padding:4px 16px;border-top:2px solid {a1};font-size:8px;color:#888;display:flex;justify-content:space-between}}
@media print{{
  @page{{size:A4 landscape;margin:10mm}}
  body{{font-size:8px}}
  #header{{padding:4px 8px}}
  td,th{{padding:2px 4px}}
  td.thumb img{{max-width:60px;max-height:36px}}
}}
"#, a1 = a1);

    // ── HTML head ─────────────────────────────────────────────────────────────
    write!(out, "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n")?;
    write!(out, "<meta charset=\"UTF-8\">\n")?;
    write!(out, "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n")?;
    write!(out, "<title>{} — Bartleby report</title>\n", he(report_name))?;
    write!(out, "<style>{}</style>\n</head>\n<body>\n", css)?;

    // ── Header ────────────────────────────────────────────────────────────────
    write!(out, "<div id=\"header\">\n")?;

    // Company block — top-left (logo, name, contact one line) — rendered first
    write!(out, "<div id=\"header-company-block\">\n")?;
    if let Some(logo_uri) = logo_data_uri(&settings.logo_path) {
        write!(out, "<div id=\"header-logo\"><img src=\"{}\" alt=\"logo\"></div>\n", logo_uri)?;
    }
    if !settings.company.is_empty() {
        write!(out, "<div id=\"header-company\">{}</div>\n", he(&settings.company.to_uppercase()))?;
    }
    let contact_line: Vec<&str> = [
        settings.contact_name.as_str(),
        settings.email.as_str(),
        settings.phone.as_str(),
    ]
    .iter()
    .filter(|s| !s.is_empty())
    .copied()
    .collect();
    if !contact_line.is_empty() {
        write!(out, "<div id=\"header-contact\">{}</div>\n", he(&contact_line.join(" / ")))?;
    }
    write!(out, "</div>\n")?; // #header-company-block

    // Project / report block — centered
    write!(out, "<div id=\"header-center\">\n")?;
    let project_display = if !settings.project_title.is_empty() {
        settings.project_title.to_uppercase()
    } else {
        report_name.to_uppercase()
    };
    write!(out, "<h1>{}</h1>\n", he(&project_display))?;
    write!(out, "<p class=\"report-line\">Backup Report &ndash; {}</p>\n", he(&now))?;
    let size_str = crate::metadata::format_size(src_total_bytes);
    write!(out, "<p class=\"src-line\">Source : {}  &ndash;  {}</p>\n",
        he(&src_path.to_string_lossy()), he(&size_str))?;
    for (i, dst) in destinations.iter().enumerate() {
        write!(out, "<p class=\"dst-line\">Destination {} : {}</p>\n",
            i + 1, he(&dst.to_string_lossy()))?;
    }
    write!(out, "</div>\n")?;

    write!(out, "</div>\n")?; // #header

    // ── Location + Comment block — BEFORE the coloured rule ──────────────────
    let has_location_or_comment = !location.is_empty() || !sanitize_comment(comment).is_empty();
    if has_location_or_comment {
        write!(out, "<div style=\"padding:6px 16px 10px;font-size:9px;line-height:1.6;\">\n")?;
        if !location.is_empty() {
            write!(out,
                "<div style=\"margin-bottom:4px;\"><span style=\"font-weight:bold;text-decoration:underline;\">Location:</span> {}</div>\n",
                he(location))?;
        }
        let safe_comment = sanitize_comment(comment);
        if !safe_comment.is_empty() {
            write!(out,
                "<div><div style=\"font-weight:bold;text-decoration:underline;margin-bottom:2px;\">Comments:</div>\
                <div>{note}</div></div>\n",
                note = safe_comment)?;
        }
        write!(out, "</div>\n")?;
    }

    // ── Table ─────────────────────────────────────────────────────────────────
    write!(out, "<table>\n<thead><tr>\n")?;
    write!(out, "<th>Preview</th>\n")?;
    if has_verify { write!(out, "<th>✓</th>\n")?; }
    if settings.col_name        { write!(out, "<th>Name</th>\n")?; }
    if settings.col_type        { write!(out, "<th>Type</th>\n")?; }
    if settings.col_size        { write!(out, "<th>Size</th>\n")?; }
    if settings.col_resolution  { write!(out, "<th>Resolution</th>\n")?; }
    if settings.col_codec       { write!(out, "<th>Codec</th>\n")?; }
    if settings.col_duration    { write!(out, "<th>Duration</th>\n")?; }
    if settings.col_bit_depth   { write!(out, "<th>Bit Depth</th>\n")?; }
    if settings.col_chroma      { write!(out, "<th>Chroma</th>\n")?; }
    if settings.col_color_space { write!(out, "<th>Color Space</th>\n")?; }
    if settings.col_sample_rate { write!(out, "<th>Sample Rate</th>\n")?; }
    if has_hash                 { write!(out, "<th>{}</th>\n", hash_col)?; }
    write!(out, "</tr></thead>\n<tbody>\n")?;

    // Count columns after the thumbnail (used for directory row colspan).
    let col_count: usize = [
        has_verify, settings.col_name, settings.col_type, settings.col_size,
        settings.col_resolution, settings.col_codec, settings.col_duration,
        settings.col_bit_depth, settings.col_chroma, settings.col_color_space,
        settings.col_sample_rate, has_hash,
    ].iter().filter(|&&b| b).count().max(1);

    // Folder SVG — same path as the app's ico-folder symbol.
    let folder_svg = format!(
        r#"<svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="{}" stroke-width="1.5" stroke-linejoin="round"><path d="M3 7a2 2 0 0 1 2-2h4.586a1 1 0 0 1 .707.293L11.707 6.707A1 1 0 0 0 12.414 7H19a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z"/></svg>"#,
        a1
    );

    // Sort entries by (directory, filename), case-insensitive.
    let mut sorted_indices: Vec<usize> = (0..entries.len()).collect();
    sorted_indices.sort_by(|&a, &b| {
        html_sort_key(&entries[a].2).cmp(&html_sort_key(&entries[b].2))
    });

    let mut current_dir: Option<String> = None;
    let mut file_row_even = true;

    for &idx in &sorted_indices {
        let (meta, hash, rel, verify_ok) = &entries[idx];

        // Compute directory component of the relative path.
        let dir = {
            let p = Path::new(rel.as_str());
            p.parent()
                .map(|parent| parent.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default()
        };

        // Directory separator row when the directory changes.
        if current_dir.as_deref() != Some(dir.as_str()) {
            current_dir = Some(dir.clone());
            let dir_label = if dir.is_empty() { "/".to_string() } else { he(&dir) };
            write!(out, "<tr class=\"dir-row\">\n")?;
            write!(out, "<td class=\"thumb\">{}</td>\n", folder_svg)?;
            write!(out, "<td colspan=\"{}\" class=\"dir-name\">{}</td>\n", col_count, dir_label)?;
            write!(out, "</tr>\n")?;
        }

        let ext = Path::new(&meta.name)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let file_path = src_path.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));

        let thumb_html = match thumb_data_uri(&file_path, &ext) {
            Some(uri) => format!("<img src=\"{}\" alt=\"\">", uri),
            None => format!(
                "<div class=\"fallback\" style=\"background:{}\"></div>",
                fallback_color(&ext)
            ),
        };

        let row_class = if file_row_even { "row-even" } else { "row-odd" };
        file_row_even = !file_row_even;
        write!(out, "<tr class=\"{}\">\n", row_class)?;
        write!(out, "<td class=\"thumb\">{}</td>\n", thumb_html)?;

        if has_verify {
            match verify_ok {
                Some(true)  => write!(out, "<td class=\"status ok\">✓</td>\n")?,
                Some(false) => write!(out, "<td class=\"status fail\">✖</td>\n")?,
                None        => write!(out, "<td class=\"status\">—</td>\n")?,
            }
        }

        if settings.col_name        { write!(out, "<td>{}</td>\n", he(&meta.name))?; }
        if settings.col_type        { write!(out, "<td>{}</td>\n", he(&meta.file_type))?; }
        if settings.col_size        { write!(out, "<td>{}</td>\n", he(&meta.size_human))?; }
        if settings.col_resolution  { write!(out, "<td>{}</td>\n", he(&meta.resolution))?; }
        if settings.col_codec       { write!(out, "<td>{}</td>\n", he(&meta.codec))?; }
        if settings.col_duration    { write!(out, "<td>{}</td>\n", he(&meta.duration))?; }
        if settings.col_bit_depth   { write!(out, "<td>{}</td>\n", he(&meta.bit_depth))?; }
        if settings.col_chroma      { write!(out, "<td>{}</td>\n", he(&meta.chroma))?; }
        if settings.col_color_space { write!(out, "<td>{}</td>\n", he(&meta.color_space))?; }
        if settings.col_sample_rate { write!(out, "<td>{}</td>\n", he(&meta.sample_rate))?; }

        if has_hash {
            write!(out, "<td class=\"hash\">{}</td>\n", he(hash))?;
        }

        write!(out, "</tr>\n")?;
    }

    write!(out, "</tbody>\n</table>\n")?;

    // ── Footer ────────────────────────────────────────────────────────────────
    write!(out, "<div class=\"rule\"></div>\n")?;
    write!(out, "<div id=\"footer\">\n")?;
    write!(out, "<span>{} file(s) — {}</span>\n", entries.len(), now)?;
    write!(out, "<span>Generated by Bartleby</span>\n")?;
    write!(out, "</div>\n</body>\n</html>\n")?;

    out.flush()?;
    Ok(())
}
