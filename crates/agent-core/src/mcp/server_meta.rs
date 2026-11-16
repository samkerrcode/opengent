//! Server identity from the MCP handshake: display title, icon and `instructions`. All of
//! it is untrusted, server-provided metadata. Icons are accepted only as small inline
//! raster images (nothing is fetched), and instructions are sanitized and bounded. The
//! instructions reach the system prompt only while they match the version the user
//! accepted, pinned on first connect like tool definitions (see `McpStore`).

use base64::Engine;
use rmcp::model::{Icon, IconTheme, Implementation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

/// Decoded size cap per icon.
pub const MAX_ICON_BYTES: usize = 16 * 1024;
/// Icons inspected per server; the rest are ignored.
const MAX_ICONS_CONSIDERED: usize = 16;
/// Stored (and displayed) instructions cap; the prompt applies its own, smaller budget.
pub const MAX_INSTRUCTIONS_CHARS: usize = 8_000;
const MAX_TITLE_CHARS: usize = 64;

/// A server's icon as `data:` URIs re-encoded from validated PNG, JPEG or WebP bytes. A
/// theme-less icon fills both slots; the UI shows the one matching the color scheme.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ServerIcon {
    /// For light backgrounds.
    pub light: Option<String>,
    /// For dark backgrounds.
    pub dark: Option<String>,
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
}

impl ServerMeta {
    pub fn from_handshake(server: Option<&Implementation>, instructions: Option<&str>) -> Self {
        Self {
            title: server
                .and_then(|s| s.title.as_deref())
                .and_then(sanitize_title),
            icon: server.and_then(|s| s.icons.as_deref()).and_then(pick_icon),
            instructions: instructions.and_then(sanitize_instructions),
        }
    }

    /// Re-applies the bounds to a value read from disk (`mcp.json` is editable).
    pub fn bounded(self) -> Self {
        let valid = |u: Option<String>| u.filter(|u| icon_from_data_uri(u).as_ref() == Some(u));
        Self {
            title: self.title.as_deref().and_then(sanitize_title),
            icon: self
                .icon
                .map(|i| ServerIcon {
                    light: valid(i.light),
                    dark: valid(i.dark),
                })
                .filter(|i| i.light.is_some() || i.dark.is_some()),
            instructions: self.instructions.as_deref().and_then(sanitize_instructions),
        }
    }
}

/// Signature an accepted version of the instructions is pinned by.
pub fn instructions_signature(text: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(text.as_bytes()))
}

/// Zero-width, bidi-control, tag and other invisible characters that can hide text from
/// the user while the model still reads it.
fn invisible(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x034F | 0x061C | 0x115F | 0x1160 | 0x17B4 | 0x17B5 | 0x180B..=0x180F
        | 0x200B..=0x200F | 0x202A..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F
        | 0xFEFF | 0xFFA0 | 0xFFF0..=0xFFFB | 0x1D173..=0x1D17A | 0xE0000..=0xE0FFF)
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
    for line in raw.split(['\n', '\r']) {
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
    let t = clean_line(&raw.replace(['\n', '\r', '\t'], " "));
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

/// Validates a base64 `data:` URI holding a PNG, JPEG or WebP image within the size cap
/// and returns it re-encoded canonically. Remote URLs and SVG are not accepted.
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
    Some(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    ))
}

/// The first valid icon per theme; theme-less icons fill whichever slot is empty.
fn pick_icon(icons: &[Icon]) -> Option<ServerIcon> {
    let (mut light, mut dark, mut any) = (None, None, None);
    for icon in icons.iter().take(MAX_ICONS_CONSIDERED) {
        let slot = match icon.theme {
            Some(IconTheme::Light) => &mut light,
            Some(IconTheme::Dark) => &mut dark,
            _ => &mut any,
        };
        if slot.is_none() {
            *slot = icon_from_data_uri(&icon.src);
        }
    }
    let icon = ServerIcon {
        light: light.or_else(|| any.clone()),
        dark: dark.or(any),
    };
    (icon.light.is_some() || icon.dark.is_some()).then_some(icon)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn data(mime: &str, bytes: &[u8]) -> String {
        format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
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
    fn icons_are_picked_per_theme() {
        let png = data("image/png", PNG);
        let themed = |theme| Icon::new(png.clone()).with_theme(theme);
        let i = pick_icon(&[themed(IconTheme::Dark)]).unwrap();
        assert_eq!((i.light, i.dark.is_some()), (None, true));
        let i = pick_icon(&[Icon::new("https://x/i.png"), Icon::new(png.clone())]).unwrap();
        assert!(i.light.is_some() && i.dark.is_some());
        assert!(pick_icon(&[Icon::new("https://x/i.png")]).is_none());
    }

    #[test]
    fn instructions_are_sanitized_and_bounded() {
        let raw = "Use list_*\u{200B} tools.\r\n\r\n\r\n\t1. Paginate\u{202E}\u{E0041}\u{7}  \n\n";
        assert_eq!(
            sanitize_instructions(raw).as_deref(),
            Some("Use list_* tools.\n\n\t1. Paginate")
        );
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
                dark: None,
            }),
            instructions: Some("hi\u{200B}".into()),
        }
        .bounded();
        assert_eq!(meta.title.as_deref(), Some("A B"));
        assert!(meta.icon.is_none());
        assert_eq!(meta.instructions.as_deref(), Some("hi"));
    }
}
