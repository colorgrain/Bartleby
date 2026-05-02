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
use std::path::Path;
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
    cmd.creation_flags(0x08000000);
    cmd
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
    let mut cmd = Command::new("ffmpeg");
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
    let mut cmd = Command::new("ffmpeg");
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

// ── Public entry point ────────────────────────────────────────────────────────

pub fn write_html(
    dst_dir:  &Path,
    src_name: &str,
    src_path: &Path,
    entries:  &[(FileMeta, String, String, String, Option<bool>)],
    settings: &Settings,
    gen_md5:  bool,
    gen_xxh:  bool,
) -> io::Result<()> {
    let out_path = dst_dir.join(format!("{}_report.html", src_name));
    let mut out  = std::fs::File::create(&out_path)?;

    let now = Local::now().format("%Y-%m-%d  %H:%M:%S").to_string();

    let (r1, g1, b1) = hex_to_rgb(&settings.accent_color_1);
    let (r2, g2, b2) = hex_to_rgb(&settings.accent_color_2);
    let a1 = format!("rgb({},{},{})", r1, g1, b1);
    let a2 = format!("rgb({},{},{})", r2, g2, b2);

    let checksum_header = if gen_md5 && gen_xxh { "Checksum" }
                          else if gen_md5        { "MD5" }
                          else                   { "XXH3" };

    let has_verify = entries.iter().any(|(_, _, _, _, ok)| ok.is_some());
    let has_hash   = gen_md5 || gen_xxh;

    // ── Inline CSS ────────────────────────────────────────────────────────────
    let css = format!(r#"
*{{box-sizing:border-box;margin:0;padding:0}}
body{{font-family:Arial,Helvetica,sans-serif;font-size:10px;color:#222;background:#fff}}
#header{{display:flex;align-items:center;justify-content:space-between;padding:8px 16px;border-bottom:2px solid {a2}}}
#header-left h1{{font-size:15px;font-weight:bold;color:#000;text-transform:uppercase}}
#header-left p.report-type{{font-size:9px;font-weight:bold;color:#333;margin-top:1px}}
#header-left p{{font-size:9px;color:#555;margin-top:2px}}
#header-center img{{max-height:48px;max-width:160px;object-fit:contain}}
#header-right{{text-align:right;font-size:9px;color:#444;line-height:1.6}}
#header-right .company-name{{font-size:12px;font-weight:bold;color:#222}}
.rule{{height:2px;background:{a2};margin:0}}
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
#footer{{margin-top:6px;padding:4px 16px;border-top:2px solid {a2};font-size:8px;color:#888;display:flex;justify-content:space-between}}
@media print{{
  @page{{size:A4 landscape;margin:10mm}}
  body{{font-size:8px}}
  #header{{padding:4px 8px}}
  td,th{{padding:2px 4px}}
  td.thumb img{{max-width:60px;max-height:36px}}
}}
"#, a1 = a1, a2 = a2);

    // ── HTML head ─────────────────────────────────────────────────────────────
    write!(out, "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n")?;
    write!(out, "<meta charset=\"UTF-8\">\n")?;
    write!(out, "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\n")?;
    write!(out, "<title>{} — Bartleby report</title>\n", he(src_name))?;
    write!(out, "<style>{}</style>\n</head>\n<body>\n", css)?;

    // ── Header ────────────────────────────────────────────────────────────────
    write!(out, "<div id=\"header\">\n")?;

    // Left: project title + source
    write!(out, "<div id=\"header-left\">\n")?;
    if !settings.project_title.is_empty() {
        write!(out, "<h1>{}</h1>\n", he(&settings.project_title.to_uppercase()))?;
    } else {
        write!(out, "<h1>{}</h1>\n", he(&src_name.to_uppercase()))?;
    }
    write!(out, "<p class=\"report-type\">BACKUP REPORT</p>\n")?;
    write!(out, "<p>Source: {}</p>\n", he(&src_path.to_string_lossy()))?;
    write!(out, "<p>Generated: {}</p>\n", now)?;
    write!(out, "</div>\n")?;

    // Centre: logo (if any)
    if let Some(logo_uri) = logo_data_uri(&settings.logo_path) {
        write!(out, "<div id=\"header-center\"><img src=\"{}\" alt=\"logo\"></div>\n", logo_uri)?;
    } else {
        write!(out, "<div id=\"header-center\"></div>\n")?;
    }

    // Right: company / contact
    write!(out, "<div id=\"header-right\">\n")?;
    if !settings.company.is_empty()      { write!(out, "<span class=\"company-name\">{}</span><br>\n", he(&settings.company))?; }
    if !settings.contact_name.is_empty() { write!(out, "{}<br>\n", he(&settings.contact_name))?; }
    if !settings.email.is_empty()        { write!(out, "{}<br>\n", he(&settings.email))?; }
    if !settings.phone.is_empty()        { write!(out, "{}<br>\n", he(&settings.phone))?; }
    write!(out, "</div>\n</div>\n")?;
    write!(out, "<div class=\"rule\"></div>\n")?;

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
    if has_hash                 { write!(out, "<th>{}</th>\n", checksum_header)?; }
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
        html_sort_key(&entries[a].3).cmp(&html_sort_key(&entries[b].3))
    });

    let mut current_dir: Option<String> = None;
    let mut file_row_even = true;

    for &idx in &sorted_indices {
        let (meta, md5, xxh3, rel, verify_ok) = &entries[idx];

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
            if gen_md5 && gen_xxh {
                write!(out, "<td class=\"hash\">{}<br>{}</td>\n", he(md5), he(xxh3))?;
            } else if gen_md5 {
                write!(out, "<td class=\"hash\">{}</td>\n", he(md5))?;
            } else {
                write!(out, "<td class=\"hash\">{}</td>\n", he(xxh3))?;
            }
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
