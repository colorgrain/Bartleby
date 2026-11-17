//! # Module `pdf_fonts`
//!
//! Fonts of the PDF report.
//!
//! The 14 PDF built-in fonts (Helvetica…) only encode WinAnsi (Western Latin):
//! any other character of a file or folder name — Polish, Czech, Greek,
//! Cyrillic, Japanese… — was silently dropped from the report. Instead:
//!
//! 1. **Noto Sans** (regular, bold, italic, bold italic; SIL OFL 1.1, see
//!    `fonts/OFL.txt`) is embedded in the application and in each PDF, subset
//!    to the glyphs actually used. It covers Latin (extended), Greek, Cyrillic
//!    and Vietnamese.
//! 2. Characters Noto Sans lacks (CJK, Arabic, Hebrew, Thai…) are drawn with a
//!    **TrueType font installed on the system** that has them (Windows: MS
//!    Gothic, Yu Gothic, Microsoft YaHei…; macOS: Arial Unicode…; Linux:
//!    Droid Sans Fallback, WenQuanYi…). printpdf can only embed TrueType
//!    (`glyf`) outlines, so CFF fonts such as Noto Sans CJK are skipped.
//! 3. A character no available font has is drawn as `?`, never dropped.
//!
//! `install` loads the fonts for one document and makes them current on the
//! calling thread; `put_text` and `width_mm` then pick the right font for each
//! run of characters. Without an installed set (unit tests), callers fall back
//! to the built-in Helvetica metrics.

use printpdf::{IndirectFontRef, Mm, PdfDocumentReference, PdfLayerReference};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static NOTO: [&[u8]; 4] = [
    include_bytes!("../fonts/NotoSans-Regular.ttf"),
    include_bytes!("../fonts/NotoSans-Bold.ttf"),
    include_bytes!("../fonts/NotoSans-Italic.ttf"),
    include_bytes!("../fonts/NotoSans-BoldItalic.ttf"),
];

/// The four fonts of a report, as handed to the drawing code.
pub struct Fonts {
    pub regular:     IndirectFontRef,
    pub bold:        IndirectFontRef,
    pub italic:      IndirectFontRef,
    pub bold_italic: IndirectFontRef,
}

struct LoadedFace {
    font: IndirectFontRef,
    face: ttf_parser::Face<'static>,
}

struct FontSet {
    styles:    Vec<LoadedFace>, // regular, bold, italic, bold italic
    fallbacks: Vec<LoadedFace>,
}

thread_local! {
    static CURRENT: RefCell<Option<FontSet>> = const { RefCell::new(None) };
}

/// Removes the thread's font set when dropped (end of `write_pdf`, or panic).
pub struct InstallGuard;
impl Drop for InstallGuard {
    fn drop(&mut self) { CURRENT.with(|c| *c.borrow_mut() = None); }
}

/// Adds Noto Sans (and, if `texts` contain characters it lacks, system
/// fallback fonts) to `doc`, and makes them current for `put_text` /
/// `width_mm` on this thread until the guard is dropped.
pub fn install(doc: &PdfDocumentReference, texts: &[&str]) -> Result<(Fonts, InstallGuard), String> {
    let mut styles = Vec::with_capacity(4);
    for bytes in NOTO {
        let font = doc.add_external_font(std::io::Cursor::new(bytes)).map_err(|e| e.to_string())?;
        let face = ttf_parser::Face::parse(bytes, 0).map_err(|e| e.to_string())?;
        styles.push(LoadedFace { font, face });
    }

    let missing: HashSet<char> = texts.iter().flat_map(|t| t.chars())
        .filter(|c| !c.is_control() && styles[0].face.glyph_index(*c).is_none())
        .collect();
    let mut fallbacks = Vec::new();
    for bytes in fallback_fonts_for(&missing) {
        let Ok(font) = doc.add_external_font(std::io::Cursor::new(bytes)) else { continue };
        let Ok(face) = ttf_parser::Face::parse(bytes, 0) else { continue };
        fallbacks.push(LoadedFace { font, face });
    }

    let fonts = Fonts {
        regular:     styles[0].font.clone(),
        bold:        styles[1].font.clone(),
        italic:      styles[2].font.clone(),
        bold_italic: styles[3].font.clone(),
    };
    CURRENT.with(|c| *c.borrow_mut() = Some(FontSet { styles, fallbacks }));
    Ok((fonts, InstallGuard))
}

/// Splits `text` into runs drawn with the same face: (style face or fallback
/// index, text). Characters no font has become '?'.
fn runs(set: &FontSet, style: usize, text: &str) -> Vec<(Option<usize>, String)> {
    let mut out: Vec<(Option<usize>, String)> = Vec::new();
    for c in text.chars() {
        let (which, ch) = if set.styles[style].face.glyph_index(c).is_some() || c.is_control() {
            (None, c)
        } else if let Some(i) = set.fallbacks.iter().position(|f| f.face.glyph_index(c).is_some()) {
            (Some(i), c)
        } else {
            (None, '?')
        };
        match out.last_mut() {
            Some((w, s)) if *w == which => s.push(ch),
            _ => out.push((which, ch.to_string())),
        }
    }
    out
}

fn face_width_mm(face: &ttf_parser::Face, text: &str, font_size_pt: f32) -> f32 {
    let upm = face.units_per_em() as f32;
    let units: f32 = text.chars().map(|c| {
        face.glyph_index(c).and_then(|g| face.glyph_hor_advance(g)).unwrap_or(0) as f32
    }).sum();
    units / upm * font_size_pt * 0.3528 // pt → mm
}

/// Rendered width of `text` in mm with the current font set, or `None` when no
/// set is installed on this thread.
pub fn width_mm(text: &str, font_size_pt: f32, bold: bool) -> Option<f32> {
    CURRENT.with(|c| {
        let c = c.borrow();
        let set = c.as_ref()?;
        let style = if bold { 1 } else { 0 };
        Some(runs(set, style, text).iter().map(|(which, s)| {
            let face = match which { None => &set.styles[style].face, Some(i) => &set.fallbacks[*i].face };
            face_width_mm(face, s, font_size_pt)
        }).sum())
    })
}

/// Draws `text` like `layer.use_text`, switching to a fallback font for the
/// characters the chosen style lacks. Without an installed set, draws as is.
pub fn put_text<S: AsRef<str>>(layer: &PdfLayerReference, text: S, size: f32, x: Mm, y: Mm,
                              font: &IndirectFontRef) {
    let text = text.as_ref();
    CURRENT.with(|c| {
        let c = c.borrow();
        let Some(set) = c.as_ref() else {
            layer.use_text(text, size, x, y, font);
            return;
        };
        let Some(style) = set.styles.iter().position(|s| s.font == *font) else {
            layer.use_text(text, size, x, y, font);
            return;
        };
        let mut cx = x.0;
        for (which, s) in runs(set, style, text) {
            let lf = match which { None => &set.styles[style], Some(i) => &set.fallbacks[i] };
            layer.use_text(s.as_str(), size, Mm(cx), y, &lf.font);
            cx += face_width_mm(&lf.face, &s, size);
        }
    });
}

// ── System fallback fonts ─────────────────────────────────────────────────────

/// Fonts already loaded by this process (kept for the next reports) and the
/// characters known to be in no installed TrueType font.
struct FallbackCache {
    loaded:      Vec<&'static [u8]>,
    uncoverable: HashSet<char>,
    scanned:     bool,
}
static CACHE: std::sync::LazyLock<Mutex<FallbackCache>> = std::sync::LazyLock::new(|| {
    Mutex::new(FallbackCache { loaded: Vec::new(), uncoverable: HashSet::new(), scanned: false })
});

/// Font files with a broad coverage, tried first (file names, any folder).
const PREFERRED: &[&str] = &[
    "msgothic.ttc", "yugothr.ttc", "yugothm.ttc", "msyh.ttc", "msjh.ttc", "malgun.ttf",
    "simsun.ttc", "arialuni.ttf", "arial unicode.ttf", "droidsansfallbackfull.ttf",
    "droidsansfallback.ttf", "wqy-microhei.ttc", "wqy-zenhei.ttc", "arial.ttf",
    "tahoma.ttf", "dejavusans.ttf",
];

fn font_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    #[cfg(target_os = "windows")]
    {
        let win = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
        v.push(PathBuf::from(win).join("Fonts"));
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            v.push(PathBuf::from(local).join("Microsoft").join("Windows").join("Fonts"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        v.push("/System/Library/Fonts".into());
        v.push("/System/Library/Fonts/Supplemental".into());
        v.push("/Library/Fonts".into());
        if let Some(h) = dirs::home_dir() { v.push(h.join("Library/Fonts")); }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        v.push("/usr/share/fonts".into());
        v.push("/usr/local/share/fonts".into());
        if let Some(h) = dirs::home_dir() {
            v.push(h.join(".local/share/fonts"));
            v.push(h.join(".fonts"));
        }
    }
    v
}

fn collect_font_files(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if depth > 4 { return; }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() { collect_font_files(&p, depth + 1, out); continue; }
        let ext = p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
        if ext == "ttf" || ext == "ttc" { out.push(p); }
    }
}

/// TrueType (`glyf`) fonts of the system covering as much of `missing` as
/// possible (at most 4). Results are cached for the process.
fn fallback_fonts_for(missing: &HashSet<char>) -> Vec<&'static [u8]> {
    if missing.is_empty() { return Vec::new(); }
    let Ok(mut cache) = CACHE.lock() else { return Vec::new() };
    let mut need: HashSet<char> = missing.iter()
        .filter(|c| !cache.uncoverable.contains(c)).copied().collect();
    let mut chosen: Vec<&'static [u8]> = Vec::new();

    let covers = |bytes: &[u8], need: &HashSet<char>| -> Vec<char> {
        match ttf_parser::Face::parse(bytes, 0) {
            Ok(f) if f.tables().glyf.is_some() =>
                need.iter().filter(|c| f.glyph_index(**c).is_some()).copied().collect(),
            _ => Vec::new(),
        }
    };

    // Fonts loaded for earlier reports first.
    for &bytes in &cache.loaded {
        if need.is_empty() { break; }
        let got = covers(bytes, &need);
        if !got.is_empty() { got.iter().for_each(|c| { need.remove(c); }); chosen.push(bytes); }
    }
    if need.is_empty() || cache.scanned {
        need.iter().for_each(|c| { cache.uncoverable.insert(*c); });
        return chosen;
    }

    // One scan of the system font folders per process: preferred names first.
    let mut files = Vec::new();
    for d in font_dirs() { collect_font_files(&d, 0, &mut files); }
    let rank = |p: &PathBuf| {
        let n = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        PREFERRED.iter().position(|x| *x == n).unwrap_or(PREFERRED.len())
    };
    files.sort_by_key(|p| (rank(p), p.clone()));
    let mut read_bytes: u64 = 0;
    for p in files {
        if need.is_empty() || chosen.len() >= 4 || read_bytes > 400 * 1024 * 1024 { break; }
        // Skip huge files (font collections of 50+ MB) to keep the scan quick.
        let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        if size == 0 || size > 40 * 1024 * 1024 { continue; }
        let Ok(bytes) = std::fs::read(&p) else { continue };
        read_bytes += size;
        let got = covers(&bytes, &need);
        if got.is_empty() { continue; }
        got.iter().for_each(|c| { need.remove(c); });
        let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
        cache.loaded.push(leaked);
        chosen.push(leaked);
    }
    cache.scanned = true;
    need.iter().for_each(|c| { cache.uncoverable.insert(*c); });
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noto_covers_european_scripts() {
        let face = ttf_parser::Face::parse(NOTO[0], 0).unwrap();
        for c in "ąęłńśźżčřšžőűœæßÉÈ ΑΒΓαβγ АБВабвЖж ạệ €…–".chars() {
            assert!(face.glyph_index(c).is_some(), "Noto Sans lacks {c:?}");
        }
        assert!(face.tables().glyf.is_some(), "must be TrueType outlines");
    }

    #[test]
    fn missing_characters_become_question_marks() {
        let doc = printpdf::PdfDocument::empty("t");
        let (_fonts, _guard) = install(&doc, &["plain"]).unwrap();
        CURRENT.with(|c| {
            let c = c.borrow();
            let set = c.as_ref().unwrap();
            let r = runs(set, 0, "A\u{10FFFD}B");
            assert_eq!(r.concat_text(), "A?B");
        });
        assert!(width_mm("Ωμέγα Жук", 8.0, false).unwrap() > 0.0);
    }

    trait Concat { fn concat_text(&self) -> String; }
    impl Concat for Vec<(Option<usize>, String)> {
        fn concat_text(&self) -> String { self.iter().map(|(_, s)| s.as_str()).collect() }
    }
}
