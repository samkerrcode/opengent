//! Server identity from the MCP handshake: display title, icon and `instructions`. All of
//! it is untrusted, server-provided metadata. Icons are accepted only as small inline
//! raster images (nothing is fetched), and instructions are sanitized and bounded. The
//! instructions reach the system prompt only while they match the version the user
//! accepted, pinned on first connect like tool definitions (see `McpStore`).

use std::sync::Arc;

use base64::Engine;
use rmcp::model::{Icon, IconTheme, Implementation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

/// Decoded size cap per icon.
pub const MAX_ICON_BYTES: usize = 16 * 1024;
/// Width and height cap per icon, read from the image header: a small, highly compressible
/// image can still declare dimensions that are expensive to decode.
pub const MAX_ICON_PIXELS: u32 = 512;
/// Icons inspected per server; the rest are ignored.
const MAX_ICONS_CONSIDERED: usize = 16;
/// Stored (and displayed) instructions cap; the prompt applies its own, smaller budget.
pub const MAX_INSTRUCTIONS_CHARS: usize = 8_000;
const MAX_TITLE_CHARS: usize = 64;

/// A server's icon as base64 `data:` URIs of validated PNG, JPEG or WebP bytes (the image
/// bytes are passed through unchanged). Each image is sent once: `src` is shown for any
/// color scheme without its own variant.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ServerIcon {
    /// Theme-less icon, used when there is no variant for the current color scheme.
    #[serde(default)]
    pub src: Option<Arc<str>>,
    /// For light backgrounds.
    #[serde(default)]
    pub light: Option<Arc<str>>,
    /// For dark backgrounds.
    #[serde(default)]
    pub dark: Option<Arc<str>>,
}

impl ServerIcon {
    /// Drops variants that repeat another, so no image is stored or sent twice. `None` when
    /// nothing is left.
    fn deduped(self) -> Option<Self> {
        let Self {
            mut src,
            mut light,
            mut dark,
        } = self;
        if light.is_some() && light == dark {
            src = light.take();
            dark = None;
        }
        if light.is_some() && dark.is_some() {
            src = None;
        }
        if light == src {
            light = None;
        }
        if dark == src {
            dark = None;
        }
        (src.is_some() || light.is_some() || dark.is_some()).then_some(Self { src, light, dark })
    }
}

/// Whether a server's instructions are added to the system prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum InstructionsStatus {
    /// Included (the accepted version).
    Included,
    /// Turned off by the user.
    Off,
    /// Changed since they were accepted; left out until the user accepts them again.
    Changed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ServerInstructions {
    /// Sanitized text, exactly as it would be sent to the model (before the prompt budget).
    pub text: String,
    pub status: InstructionsStatus,
}

/// Handshake metadata kept with the live session and cached in `mcp.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ServerMeta {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub icon: Option<ServerIcon>,
    /// Sanitized and bounded (`sanitize_instructions`).
    #[serde(default)]
    pub instructions: Option<String>,
    /// `instructions_signature` of `instructions` (of `""` when there are none), computed
    /// once by the constructors. Not persisted.
    #[serde(skip)]
    signature: String,
}

impl ServerMeta {
    pub fn from_handshake(server: Option<&Implementation>, instructions: Option<&str>) -> Self {
        Self::new(
            server
                .and_then(|s| s.title.as_deref())
                .and_then(sanitize_title),
            server.and_then(|s| s.icons.as_deref()).and_then(pick_icon),
            instructions.and_then(sanitize_instructions),
        )
    }

    fn new(title: Option<String>, icon: Option<ServerIcon>, instructions: Option<String>) -> Self {
        Self {
            signature: instructions_signature(instructions.as_deref().unwrap_or_default()),
            title,
            icon,
            instructions,
        }
    }

    /// Re-applies the bounds to a value read from disk (`mcp.json` is editable).
    pub fn bounded(self) -> Self {
        let valid =
            |u: Option<Arc<str>>| u.filter(|u| icon_from_data_uri(u).as_deref() == Some(&**u));
        Self::new(
            self.title.as_deref().and_then(sanitize_title),
            self.icon.and_then(|i| {
                ServerIcon {
                    src: valid(i.src),
                    light: valid(i.light),
                    dark: valid(i.dark),
                }
                .deduped()
            }),
            self.instructions.as_deref().and_then(sanitize_instructions),
        )
    }

    /// The signature the current instructions are pinned by. Servers without instructions
    /// have one too, so instructions they add later need review like a changed version.
    pub fn signature(&self) -> &str {
        &self.signature
    }
}

/// Signature an accepted version of the instructions is pinned by.
pub fn instructions_signature(text: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(text.as_bytes()))
}

/// Zero-width, bidi-control, tag and other invisible characters that can hide text from
/// the user while the model still reads it. Stripping the joiners (U+200C/U+200D) and
/// variation selectors is deliberate: it breaks emoji sequences and some Persian and Indic
/// shaping, an acceptable cost for untrusted text. U+2028/U+2029 are line breaks for
/// instructions (`sanitize_instructions` splits on them first) and spaces in titles.
fn invisible(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x034F | 0x061C | 0x115F | 0x1160 | 0x17B4 | 0x17B5 | 0x180B..=0x180F
        | 0x200B..=0x200F | 0x2028 | 0x2029 | 0x202A..=0x202E | 0x2060..=0x206F | 0x3164
        | 0xFE00..=0xFE0F | 0xFEFF | 0xFFA0 | 0xFFF0..=0xFFFB | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A | 0xE0000..=0xE0FFF)
}

fn clean_line(line: &str) -> String {
    line.chars()
        .filter(|&c| c == '\t' || !(c.is_control() || invisible(c)))
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// Strips control and invisible characters, normalizes line endings, collapses runs of
/// blank lines and bounds the length. `None` when nothing is left.
pub fn sanitize_instructions(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = 0;
    let mut blank = false;
    // `lines` takes `\n` and `\r\n` as one break; a lone `\r`, U+2028 or U+2029 is one too.
    let lines = raw
        .lines()
        .flat_map(|l| l.split(['\r', '\u{2028}', '\u{2029}']));
    for line in lines {
        let line = clean_line(line);
        if line.trim().is_empty() {
            if !out.is_empty() {
                blank = true;
            }
            continue;
        }
        let sep = if out.is_empty() {
            ""
        } else if blank {
            "\n\n"
        } else {
            "\n"
        };
        blank = false;
        let room = MAX_INSTRUCTIONS_CHARS.saturating_sub(chars + sep.len());
        if room == 0 {
            break;
        }
        out.push_str(sep);
        let piece: String = line.chars().take(room).collect();
        chars += sep.len() + piece.chars().count();
        out.push_str(&piece);
    }
    (!out.is_empty()).then_some(out)
}

fn sanitize_title(raw: &str) -> Option<String> {
    let t = clean_line(&raw.replace(['\n', '\r', '\t', '\u{2028}', '\u{2029}'], " "));
    let t: String = t.trim().chars().take(MAX_TITLE_CHARS).collect();
    (!t.is_empty()).then_some(t)
}

/// The image type of `bytes`, by magic number.
fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?).into())
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?).into())
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    let s = b.get(at..at + 3)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], 0]))
}

/// Width and height declared in the header of a sniffed image (`None` if it is malformed):
/// PNG `IHDR`, the first JPEG start-of-frame, or the WebP `VP8 `/`VP8L`/`VP8X` header.
fn dimensions(mime: &str, b: &[u8]) -> Option<(u32, u32)> {
    match mime {
        "image/png" => {
            if b.get(12..16)? != b"IHDR" {
                return None;
            }
            let be32 = |at: usize| Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?));
            Some((be32(16)?, be32(20)?))
        }
        "image/jpeg" => {
            let mut i = 2;
            loop {
                if *b.get(i)? != 0xFF {
                    return None;
                }
                match *b.get(i + 1)? {
                    0xFF => i += 1,
                    0x01 | 0xD0..=0xD7 => i += 2,
                    // Start of frame, except DHT (C4), JPG (C8) and DAC (CC).
                    m @ 0xC0..=0xCF if !matches!(m, 0xC4 | 0xC8 | 0xCC) => {
                        return Some((be16(b, i + 7)?, be16(b, i + 5)?));
                    }
                    // Image data (or its end) before any frame header.
                    0xD8..=0xDA => return None,
                    _ => i += 2 + usize::try_from(be16(b, i + 2)?).ok()?,
                }
            }
        }
        "image/webp" => match b.get(12..16)? {
            b"VP8 " if b.get(23..26)? == [0x9D, 0x01, 0x2A] => {
                Some((le16(b, 26)? & 0x3FFF, le16(b, 28)? & 0x3FFF))
            }
            b"VP8L" if *b.get(20)? == 0x2F => {
                let bits = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                Some(((bits & 0x3FFF) + 1, ((bits >> 14) & 0x3FFF) + 1))
            }
            b"VP8X" => Some((le24(b, 24)? + 1, le24(b, 27)? + 1)),
            _ => None,
        },
        _ => None,
    }
}

/// Validates a base64 `data:` URI holding a PNG, JPEG or WebP image within the size and
/// dimension caps and returns it in canonical form (the image bytes are unchanged). Remote
/// URLs and SVG are not accepted.
fn icon_from_data_uri(src: &str) -> Option<String> {
    let rest = src.trim().strip_prefix("data:")?;
    let (header, payload) = rest.split_once(',')?;
    let mut params = header.split(';').map(str::trim);
    let declared = params.next()?.to_ascii_lowercase();
    if !params.any(|p| p.eq_ignore_ascii_case("base64")) {
        return None;
    }
    if !matches!(
        declared.as_str(),
        "image/png" | "image/jpeg" | "image/jpg" | "image/webp"
    ) || payload.len() > MAX_ICON_BYTES.div_ceil(3) * 4 + 4
    {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .ok()?;
    if bytes.is_empty() || bytes.len() > MAX_ICON_BYTES {
        return None;
    }
    let mime = sniff(&bytes)?;
    let (w, h) = dimensions(mime, &bytes)?;
    if w == 0 || h == 0 || w > MAX_ICON_PIXELS || h > MAX_ICON_PIXELS {
        return None;
    }
    Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    ))
}

/// The first valid icon per theme and the first theme-less one.
fn pick_icon(icons: &[Icon]) -> Option<ServerIcon> {
    let mut icon = ServerIcon::default();
    for i in icons.iter().take(MAX_ICONS_CONSIDERED) {
        let slot = match i.theme {
            Some(IconTheme::Light) => &mut icon.light,
            Some(IconTheme::Dark) => &mut icon.dark,
            _ => &mut icon.src,
        };
        if slot.is_none() {
            *slot = icon_from_data_uri(&i.src).map(Arc::from);
        }
    }
    icon.deduped()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16×16 and 32×32 PNG headers.
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x10\0\0\0\x10";
    const PNG2: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x20\0\0\0\x20";

    fn data(mime: &str, bytes: &[u8]) -> String {
        format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    }

    fn webp(chunk: &[u8; 4], body: &[u8]) -> Vec<u8> {
        [b"RIFF\0\0\0\0WEBP".as_slice(), chunk, b"\0\0\0\0", body].concat()
    }

    #[test]
    fn icon_dimensions_are_read_from_headers() {
        let dims = |b: &[u8]| sniff(b).and_then(|m| dimensions(m, b));
        assert_eq!(dims(PNG), Some((16, 16)));
        assert_eq!(dims(&PNG[..20]), None);
        // APP0 segment, then a baseline frame header: 32 high, 64 wide.
        let jpeg = b"\xFF\xD8\xFF\xE0\0\x04JF\xFF\xC0\0\x11\x08\0\x20\0\x40\x03";
        assert_eq!(dims(jpeg), Some((64, 32)));
        assert_eq!(dims(b"\xFF\xD8\xFF\xDA\0\x02"), None, "scan before frame");
        assert_eq!(
            dims(&webp(b"VP8 ", b"\0\0\0\x9D\x01\x2A\x30\0\x40\0")),
            Some((48, 64))
        );
        assert_eq!(dims(&webp(b"VP8L", b"\x2F\x0F\xC0\x03\0")), Some((16, 16)));
        assert_eq!(
            dims(&webp(b"VP8X", b"\0\0\0\0\xE7\x03\0\0\0\0")),
            Some((1000, 1))
        );
        assert_eq!(dims(&webp(b"ALPH", b"\0\0\0\0\0\0\0\0\0\0")), None);
    }

    #[test]
    fn icons_over_the_pixel_cap_are_rejected() {
        let mut huge = PNG.to_vec();
        huge[16..20].copy_from_slice(&(MAX_ICON_PIXELS + 1).to_be_bytes());
        assert!(icon_from_data_uri(&data("image/png", &huge)).is_none());
        let mut empty = PNG.to_vec();
        empty[20..24].copy_from_slice(&0u32.to_be_bytes());
        assert!(icon_from_data_uri(&data("image/png", &empty)).is_none());
        let vp8x = webp(b"VP8X", b"\0\0\0\0\xE7\x03\0\0\0\0");
        assert!(icon_from_data_uri(&data("image/webp", &vp8x)).is_none());
        let vp8l = webp(b"VP8L", b"\x2F\x0F\xC0\x03\0");
        assert!(icon_from_data_uri(&data("image/webp", &vp8l)).is_some());
    }

    #[test]
    fn icons_accept_only_small_inline_rasters() {
        let png = data("image/png", PNG);
        assert_eq!(icon_from_data_uri(&png).as_deref(), Some(png.as_str()));
        // Declared type is checked against the bytes.
        assert_eq!(
            icon_from_data_uri(&data("image/jpeg", PNG)).as_deref(),
            Some(png.as_str())
        );
        assert!(icon_from_data_uri(&data("image/svg+xml", b"<svg/>")).is_none());
        assert!(icon_from_data_uri(&data("image/png", b"<svg/>")).is_none());
        assert!(icon_from_data_uri("https://example.com/icon.png").is_none());
        assert!(icon_from_data_uri("data:image/png,raw").is_none());
        let mut big = PNG.to_vec();
        big.resize(MAX_ICON_BYTES + 1, 0);
        assert!(icon_from_data_uri(&data("image/png", &big)).is_none());
    }

    #[test]
    fn icons_are_picked_per_theme_and_sent_once() {
        let (a, b) = (data("image/png", PNG), data("image/png", PNG2));
        let themed = |src: &str, theme| Icon::new(src).with_theme(theme);
        let i = pick_icon(&[themed(&a, IconTheme::Dark)]).unwrap();
        assert_eq!((i.src, i.light, i.dark.as_deref()), (None, None, Some(&*a)));
        // A theme-less icon is sent once, not copied into both variants.
        let i = pick_icon(&[Icon::new("https://x/i.png"), Icon::new(a.clone())]).unwrap();
        assert_eq!((i.src.as_deref(), i.light, i.dark), (Some(&*a), None, None));
        // Identical variants collapse; a theme-less icon both variants cover is dropped.
        let i = pick_icon(&[themed(&a, IconTheme::Light), themed(&a, IconTheme::Dark)]).unwrap();
        assert_eq!((i.src.as_deref(), i.light, i.dark), (Some(&*a), None, None));
        let i = pick_icon(&[
            Icon::new(a.clone()),
            themed(&a, IconTheme::Light),
            themed(&b, IconTheme::Dark),
        ])
        .unwrap();
        assert_eq!(
            (i.src, i.light.as_deref(), i.dark.as_deref()),
            (None, Some(&*a), Some(&*b))
        );
        let i = pick_icon(&[Icon::new(a.clone()), themed(&b, IconTheme::Dark)]).unwrap();
        assert_eq!(
            (i.src.as_deref(), i.light, i.dark.as_deref()),
            (Some(&*a), None, Some(&*b))
        );
        assert!(pick_icon(&[Icon::new("https://x/i.png")]).is_none());
    }

    #[test]
    fn instructions_are_sanitized_and_bounded() {
        let raw = "Use list_*\u{200B} tools.\r\n\r\n\r\n\t1. Paginate\u{202E}\u{E0041}\u{7}  \n\n";
        assert_eq!(
            sanitize_instructions(raw).as_deref(),
            Some("Use list_* tools.\n\n\t1. Paginate")
        );
        // Single CRLFs, lone CRs and Unicode separators are line breaks, not paragraphs.
        assert_eq!(
            sanitize_instructions("a\r\nb\rc\u{2028}d\u{2029}e\r\n\r\nf").as_deref(),
            Some("a\nb\nc\nd\ne\n\nf")
        );
        assert_eq!(sanitize_instructions("x\u{1BCA0}y").as_deref(), Some("xy"));
        assert!(sanitize_instructions(" \n\u{200B}\n").is_none());
        let long = sanitize_instructions(&"x".repeat(MAX_INSTRUCTIONS_CHARS * 2)).unwrap();
        assert_eq!(long.chars().count(), MAX_INSTRUCTIONS_CHARS);
    }

    #[test]
    fn bounded_drops_tampered_cache_entries() {
        let meta = ServerMeta {
            title: Some("A\nB".into()),
            icon: Some(ServerIcon {
                light: Some("data:image/svg+xml;base64,PHN2Zy8+".into()),
                ..Default::default()
            }),
            instructions: Some("hi\u{200B}".into()),
            signature: "forged".into(),
        }
        .bounded();
        assert_eq!(meta.title.as_deref(), Some("A B"));
        assert!(meta.icon.is_none());
        assert_eq!(meta.instructions.as_deref(), Some("hi"));
        assert_eq!(meta.signature(), instructions_signature("hi"));
        // A cached icon stored in both variants is kept once.
        let png: Arc<str> = data("image/png", PNG).into();
        let icon = ServerMeta {
            icon: Some(ServerIcon {
                light: Some(png.clone()),
                dark: Some(png.clone()),
                ..Default::default()
            }),
            ..Default::default()
        }
        .bounded()
        .icon
        .unwrap();
        assert_eq!((icon.src, icon.light, icon.dark), (Some(png), None, None));
    }

    #[test]
    fn servers_without_instructions_have_a_signature() {
        let none = ServerMeta::from_handshake(None, None);
        assert_eq!(none.signature(), instructions_signature(""));
        assert_ne!(
            none.signature(),
            ServerMeta::from_handshake(None, Some("v1")).signature()
        );
    }
}
