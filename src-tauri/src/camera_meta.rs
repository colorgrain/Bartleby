//! # Module `camera_meta`
//!
//! Camera-acquisition metadata (gamut, log curve, camera model, ISO, white
//! balance) that `mediainfo` either does not expose or reports incorrectly.
//!
//! ## Guiding rule: read, never assume
//! Every value returned here was **read from the file or its sidecar**. A value
//! the file does not contain stays empty. Vendor codes are only translated when
//! the translation was confirmed on a real sample (see the tables below); an
//! unknown code is reported verbatim as `UL <hex>` so nothing is invented.
//!
//! ## Sources, in order of trust
//! 1. **Sony XML sidecar** (`<clip>M01.XML`, written by the camera next to the
//!    MXF): `CaptureColorPrimaries`, `CaptureGammaEquation`, `Device/@modelName`.
//! 2. **MXF picture descriptor** (header partition): the `ColorPrimaries`
//!    (local tag 0x3219) and `TransferCharacteristic` (0x3210) universal labels.
//!    Cameras write vendor-private labels there (S-Gamut3.Cine, LogC4…).
//!    mediainfo prints those as raw hex or, worse, mis-maps them — an ALEXA 35
//!    AWG4 label is shown as "BT.601 PAL" by mediainfo 26.10.
//! 3. **R3D header**: a tag/length/value list (camera, ISO, white balance,
//!    resolution, REDCODE quality). Gamut and gamma are stored as numeric codes
//!    whose meaning is not publicly documented, so they are not decoded.
//!
//! The mediainfo-based sources (ARRI camera metadata, RDD 18 acquisition
//! metadata, QuickTime `mdta` keys) are read in `metadata.rs` from its JSON output.

use std::io::Read;
use std::path::Path;

/// Values gathered from vendor-specific sources. Empty string = not found.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct CameraMeta {
    pub gamut:         String,
    pub transfer:      String,
    pub camera:        String,
    pub iso:           String,
    pub white_balance: String,
    /// Resolution "WxH" (R3D only — mediainfo does not parse the R3D header).
    pub resolution:    String,
    /// Codec label (R3D only: "REDCODE ELQ").
    pub codec:         String,
}

// ── MXF universal labels ──────────────────────────────────────────────────────
//
// Only the last 8 bytes of the 16-byte label are compared; the first 8 bytes
// are the SMPTE prefix `06 0E 2B 34 04 01 01 xx` (xx = registry version, which
// differs between vendors and must be ignored).
//
// Every entry below was confirmed on a sample file whose settings were known:
// - Sony PXW-FS5 XAVC, S-Gamut3.Cine / S-Log3 (matching camera XML sidecar).
// - ARRI ALEXA 35 ProRes 4444 XQ, AWG4 / LogC4 (matching ARRI camera metadata).
// - Canon EOS C80 XF-AVC, Cinema Gamut / Canon Log 2 (as read by DaVinci Resolve,
//   confirmed by the user; the card's CPF picture profile is named "Canon Log 2").

/// Private colour-primaries labels → display name.
const MXF_PRIMARIES: &[([u8; 8], &str)] = &[
    ([0x0E, 0x06, 0x04, 0x01, 0x01, 0x01, 0x03, 0x05], "S-Gamut3.Cine"),
    ([0x0E, 0x17, 0x01, 0x02, 0x03, 0x02, 0x00, 0x00], "AWG4"),
    ([0x0E, 0x15, 0x00, 0x02, 0x02, 0x00, 0x00, 0x00], "Cinema Gamut"),
];

/// Private transfer-characteristic labels → display name.
const MXF_TRANSFER: &[([u8; 8], &str)] = &[
    ([0x0E, 0x06, 0x04, 0x01, 0x01, 0x01, 0x06, 0x05], "S-Log3"),
    ([0x0E, 0x17, 0x01, 0x02, 0x04, 0x02, 0x00, 0x00], "LogC4"),
    ([0x0E, 0x15, 0x00, 0x01, 0x02, 0x00, 0x00, 0x00], "Canon Log 2"),
];

/// How many bytes of the MXF header partition are scanned. The picture
/// descriptor sits in the header metadata, within the first few hundred KB.
const MXF_SCAN_BYTES: u64 = 2 * 1024 * 1024;

/// Reads the private gamut / transfer labels of an MXF file.
///
/// Only **private** labels (byte 8 = `0x0E`) are returned: standard SMPTE
/// labels (BT.709, BT.2020, PQ, HLG…) are already named correctly by mediainfo.
/// Unknown private labels are returned as `"UL 0E15000102000000"`.
pub fn read_mxf(path: &Path) -> CameraMeta {
    let mut meta = CameraMeta::default();
    let Some(buf) = read_head(path, MXF_SCAN_BYTES) else { return meta; };
    if let Some(ul) = find_local_ul(&buf, [0x32, 0x19]) {
        meta.gamut = name_ul(&ul, MXF_PRIMARIES).unwrap_or_default();
    }
    if let Some(ul) = find_local_ul(&buf, [0x32, 0x10]) {
        meta.transfer = name_ul(&ul, MXF_TRANSFER).unwrap_or_default();
    }
    meta
}

/// Finds a UL-valued local-set item: `tag(2) | length 0x0010 | 06 0E 2B 34 …`.
/// Returns the last 8 bytes of the 16-byte label.
fn find_local_ul(buf: &[u8], tag: [u8; 2]) -> Option<[u8; 8]> {
    let pat = [tag[0], tag[1], 0x00, 0x10, 0x06, 0x0E, 0x2B, 0x34];
    let i = buf.windows(pat.len()).position(|w| w == pat)?;
    let start = i + 4 + 8; // skip tag + length + SMPTE prefix
    buf.get(start..start + 8)?.try_into().ok()
}

/// Translates a private label, or formats an unknown one. `None` for a
/// standard (non-private) label.
fn name_ul(ul: &[u8; 8], table: &[([u8; 8], &str)]) -> Option<String> {
    if ul[0] != 0x0E { return None; }
    if let Some((_, name)) = table.iter().find(|(k, _)| k == ul) {
        return Some((*name).to_string());
    }
    let hex: String = ul.iter().map(|b| format!("{:02X}", b)).collect();
    Some(format!("UL {}", hex))
}

// ── Sony XML sidecar ──────────────────────────────────────────────────────────

/// Reads the Sony `NonRealTimeMeta` sidecar written next to an MXF clip
/// (`Clip0054.MXF` → `Clip0054M01.XML`). Returns `None` if there is none.
pub fn read_sony_xml(mxf: &Path) -> Option<CameraMeta> {
    let stem = mxf.file_stem()?.to_string_lossy().to_string();
    let dir  = mxf.parent()?;
    let xml = ["M01.XML", "M01.xml"].iter()
        .map(|suffix| dir.join(format!("{}{}", stem, suffix)))
        .find(|p| p.is_file())?;
    // Bounded read: a real sidecar is a few KB; a huge file that only happens
    // to carry this name must not be loaded whole (once per rayon thread).
    let mut buf = Vec::new();
    {
        use std::io::Read;
        std::fs::File::open(&xml).ok()?.take(1024 * 1024).read_to_end(&mut buf).ok()?;
    }
    let text = String::from_utf8_lossy(&buf);
    if !text.contains("NonRealTimeMeta") { return None; }

    let mut meta = CameraMeta::default();
    if let Some(v) = xml_item(&text, "CaptureColorPrimaries") { meta.gamut = sony_name(&v); }
    if let Some(v) = xml_item(&text, "CaptureGammaEquation")  { meta.transfer = sony_name(&v); }
    if let Some(model) = xml_attr(&text, "<Device", "modelName") {
        let make = xml_attr(&text, "<Device", "manufacturer").unwrap_or_default();
        meta.camera = join_make_model(&make, &model);
    }
    Some(meta)
}

/// Value of `<Item name="{name}" value="…"/>`.
fn xml_item(text: &str, name: &str) -> Option<String> {
    let key = format!("name=\"{}\"", name);
    let i = text.find(&key)?;
    let line_end = text[i..].find('>').map(|e| i + e)?;
    attr_in(&text[i..line_end], "value")
}

/// Value of `attr="…"` inside the first element starting with `elem`.
fn xml_attr(text: &str, elem: &str, attr: &str) -> Option<String> {
    let i = text.find(elem)?;
    let end = text[i..].find('>').map(|e| i + e)?;
    attr_in(&text[i..end], attr)
}

fn attr_in(fragment: &str, attr: &str) -> Option<String> {
    let key = format!("{}=\"", attr);
    let s = fragment.find(&key)? + key.len();
    let e = fragment[s..].find('"')? + s;
    Some(fragment[s..e].trim().to_string()).filter(|v| !v.is_empty())
}

/// Sony XML identifiers → the names printed on the camera menus.
/// `s-log3-cine` is how Sony labels S-Log3 when paired with S-Gamut3.Cine;
/// the curve itself is S-Log3. Unknown identifiers are returned unchanged.
fn sony_name(v: &str) -> String {
    match v.to_lowercase().as_str() {
        "s-gamut3-cine" => "S-Gamut3.Cine".into(),
        "s-gamut3"      => "S-Gamut3".into(),
        "s-gamut"       => "S-Gamut".into(),
        "s-log3-cine" | "s-log3" => "S-Log3".into(),
        "s-log2"        => "S-Log2".into(),
        "rec709"        => "BT.709".into(),
        "rec2020"       => "BT.2020".into(),
        _ => v.to_string(),
    }
}

// ── RED R3D header ────────────────────────────────────────────────────────────
//
// Layout: u32 BE header length, "RED2", then (from offset 0x4D on the samples)
// a list of items `u16 BE length (incl. these 4 bytes) | u16 BE tag | value`.
// The high nibble of the tag gives the value type: 1 = string, 2 = f32,
// 4 = u16 (or array of u16). Tags used (cross-checked with the V-RAPTOR sample):
//   0x10A0 camera model ("V-RAPTOR XE 8K VV")
//   0x10BE REDCODE quality ("ELQ")
//   0x403B ISO (u16)
//   0x200D white balance in kelvin (f32)
//   0x4037 crop rectangle, 4 × u16: x, y, width, height

/// Parses the R3D clip header. Returns `None` if the file is not an R3D.
pub fn read_r3d(path: &Path) -> Option<CameraMeta> {
    let buf = read_head(path, 64 * 1024)?;
    if buf.get(4..8)? != b"RED2" { return None; }
    let hdr_len = u32::from_be_bytes(buf.get(0..4)?.try_into().ok()?) as usize;
    let end = hdr_len.min(buf.len());

    // The item list does not start at a documented offset: take the start
    // position that yields the longest valid chain of items.
    let items = (0x40..0x100.min(end))
        .map(|s| walk_r3d(&buf, s, end))
        .max_by_key(|v| v.len())?;
    if items.len() < 8 { return None; }

    let mut meta = CameraMeta::default();
    for (tag, val) in items {
        match tag {
            0x10A0 => meta.camera = r3d_str(val),
            0x10BE => {
                let q = r3d_str(val);
                if !q.is_empty() { meta.codec = format!("REDCODE {}", q); }
            }
            0x403B if val.len() == 2 => {
                meta.iso = u16::from_be_bytes([val[0], val[1]]).to_string();
            }
            0x200D if val.len() == 4 => {
                let k = f32::from_be_bytes([val[0], val[1], val[2], val[3]]);
                if k.is_finite() && k > 1000.0 && k < 20000.0 {
                    meta.white_balance = format!("{:.0}K", k);
                }
            }
            0x4037 if val.len() == 8 => {
                let w = u16::from_be_bytes([val[4], val[5]]);
                let h = u16::from_be_bytes([val[6], val[7]]);
                if w > 0 && h > 0 { meta.resolution = format!("{}x{}", w, h); }
            }
            _ => {}
        }
    }
    if !meta.camera.is_empty() { meta.camera = join_make_model("RED", &meta.camera); }
    Some(meta)
}

fn walk_r3d(buf: &[u8], mut p: usize, end: usize) -> Vec<(u16, &[u8])> {
    let mut out = Vec::new();
    while p + 4 <= end {
        let len = u16::from_be_bytes([buf[p], buf[p + 1]]) as usize;
        let tag = u16::from_be_bytes([buf[p + 2], buf[p + 3]]);
        if len < 4 || p + len > end { break; }
        out.push((tag, &buf[p + 4..p + len]));
        p += len;
    }
    out
}

fn r3d_str(v: &[u8]) -> String {
    String::from_utf8_lossy(v).trim_end_matches('\0').trim().to_string()
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn read_head(path: &Path, n: u64) -> Option<Vec<u8>> {
    let f = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    f.take(n).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// "Sony" + "PXW-FS5" → "Sony PXW-FS5"; avoids "ARRI ARRI ALEXA 35".
pub fn join_make_model(make: &str, model: &str) -> String {
    let (make, model) = (make.trim(), model.trim());
    if make.is_empty() { return model.to_string(); }
    if model.is_empty() { return make.to_string(); }
    if model.to_lowercase().starts_with(&make.to_lowercase()) {
        model.to_string()
    } else {
        format!("{} {}", make, model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mxf_labels() {
        let mut buf = vec![0u8; 32];
        buf.extend_from_slice(&[0x32, 0x19, 0x00, 0x10, 0x06, 0x0E, 0x2B, 0x34,
                                0x04, 0x01, 0x01, 0x06,
                                0x0E, 0x06, 0x04, 0x01, 0x01, 0x01, 0x03, 0x05]);
        let ul = find_local_ul(&buf, [0x32, 0x19]).unwrap();
        assert_eq!(name_ul(&ul, MXF_PRIMARIES).as_deref(), Some("S-Gamut3.Cine"));
        let canon = [0x0E, 0x15, 0x00, 0x01, 0x02, 0x00, 0x00, 0x00];
        assert_eq!(name_ul(&canon, MXF_TRANSFER).as_deref(), Some("Canon Log 2"));
        let unknown = [0x0E, 0x15, 0x00, 0x01, 0x07, 0x00, 0x00, 0x00];
        assert_eq!(name_ul(&unknown, MXF_TRANSFER).as_deref(), Some("UL 0E15000107000000"));
        let bt709 = [0x04, 0x01, 0x01, 0x01, 0x01, 0x02, 0x00, 0x00];
        assert_eq!(name_ul(&bt709, MXF_TRANSFER), None);
    }

    #[test]
    fn sony_xml_parsing() {
        let xml = r#"<NonRealTimeMeta><Device manufacturer="Sony" modelName="PXW-FS5" serialNo="1"></Device>
            <Item name="CaptureGammaEquation" value="s-log3-cine"/>
            <Item name="CaptureColorPrimaries" value="s-gamut3-cine"/>"#;
        assert_eq!(xml_item(xml, "CaptureGammaEquation").as_deref(), Some("s-log3-cine"));
        assert_eq!(sony_name("s-gamut3-cine"), "S-Gamut3.Cine");
        assert_eq!(xml_attr(xml, "<Device", "modelName").as_deref(), Some("PXW-FS5"));
    }

    #[test]
    fn make_model() {
        assert_eq!(join_make_model("ARRI", "ARRI ALEXA 35"), "ARRI ALEXA 35");
        assert_eq!(join_make_model("Sony", "PXW-FS5"), "Sony PXW-FS5");
        assert_eq!(join_make_model("", "Shogun7"), "Shogun7");
    }
}
