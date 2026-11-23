//! Terminal output. Model output, tool names, arguments and server-supplied text are
//! untrusted: written to a terminal they are stripped of control characters and escape
//! sequences, so a tool can't disguise what an approval prompt asks for or rewrite earlier
//! lines. Piped stdout and `--json` pass through unmodified (JSON escapes control
//! characters itself).

use std::io::{IsTerminal, Write};

/// Longest argument preview shown in an approval prompt.
const MAX_ARGS_PREVIEW: usize = 2_000;

/// Removes control characters (C0, DEL, C1 — which covers ESC and CSI, so no escape
/// sequence survives), bidirectional controls that reorder how text displays, and the
/// Unicode line and paragraph separators some terminals break lines on. `\n` and `\t` are
/// kept for multi-line text; single-line text (names, ids, argument previews) also loses
/// invisible characters, so two names that look the same are the same.
pub fn sanitize(s: &str, multiline: bool) -> String {
    s.chars()
        .filter(|&c| {
            let layout = multiline && (c == '\n' || c == '\t');
            (layout || !c.is_control())
                && !is_bidi_control(c)
                && !matches!(c, '\u{2028}' | '\u{2029}')
                && (multiline || !is_invisible(c))
        })
        .collect()
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Soft hyphen, zero-width characters, invisible operators, the byte-order mark and tag
/// characters.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{00AD}' | '\u{200B}'..='\u{200D}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' | '\u{E0000}'..='\u{E007F}')
}

/// Sanitized for a terminal, else unchanged.
pub fn for_stream(s: &str, terminal: bool, multiline: bool) -> String {
    if terminal {
        sanitize(s, multiline)
    } else {
        s.to_owned()
    }
}

/// `server/tool` as shown to the user.
pub fn tool_label(server: Option<&str>, tool: &str) -> String {
    match server {
        Some(s) => format!("{}/{}", sanitize(s, false), sanitize(tool, false)),
        None => sanitize(tool, false),
    }
}

/// The approval question for a tool call. Every untrusted part is sanitized; arguments are
/// shown as compact JSON (strings stay escaped) and truncated, saying how much is hidden so
/// nobody approves a call believing they saw all of it.
pub fn approval_prompt(server: Option<&str>, tool: &str, arguments: &serde_json::Value) -> String {
    let args = serde_json::to_string(arguments).unwrap_or_default();
    let mut preview = sanitize(&args, false);
    let total = preview.chars().count();
    if total > MAX_ARGS_PREVIEW {
        preview = format!(
            "{}… ({} more characters not shown)",
            preview.chars().take(MAX_ARGS_PREVIEW).collect::<String>(),
            total - MAX_ARGS_PREVIEW
        );
    }
    format!(
        "\nAllow tool {}?\n  arguments: {preview}\n  [y] allow once   [N] deny: ",
        tool_label(server, tool)
    )
}

/// Whether both stdin and stderr are terminals, i.e. someone can answer a prompt.
pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

/// Writes a line to stderr, sanitized when stderr is a terminal.
pub fn note(msg: &str) {
    let terminal = std::io::stderr().is_terminal();
    let mut err = std::io::stderr().lock();
    let _ = writeln!(err, "{}", for_stream(msg, terminal, true));
}

/// Writes a command's result to stdout. `print!` panics when stdout is a closed pipe
/// (`| head`); here that is not an error — nobody is left to read the rest.
pub fn out(text: &str) -> crate::Result<()> {
    let mut stdout = std::io::stdout().lock();
    match stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
    {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => Err(crate::CliError::Failed(
            format!("couldn't write the output: {e}"),
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_sequences_and_controls_are_removed() {
        let hostile = "ok\x1b[2K\x1b[1A\rDeleted\x07\u{9b}31m\u{202e}txt.exe\u{0}";
        let clean = sanitize(hostile, false);
        assert!(!clean.chars().any(|c| c.is_control()), "{clean:?}");
        assert!(!clean.contains('\u{202e}'));
        assert_eq!(clean, "ok[2K[1ADeleted31mtxt.exe");
        assert_eq!(sanitize("a\nb\tc\r", true), "a\nb\tc");
        assert_eq!(sanitize("a\nb", false), "ab");
    }

    #[test]
    fn invisible_and_line_breaking_characters_are_removed() {
        // Arabic letter mark (bidi), line/paragraph separators: never shown.
        assert_eq!(sanitize("a\u{61c}b\u{2028}c\u{2029}d", true), "abcd");
        // A name can't hide characters; prose keeps joiners (emoji sequences).
        let name = "re\u{200b}ad\u{feff}\u{ad}\u{2060}\u{e0041}";
        assert_eq!(sanitize(name, false), "read");
        assert_eq!(tool_label(Some("f\u{200d}s"), name), "fs/read");
        assert_eq!(sanitize("a\u{200d}b", true), "a\u{200d}b");
    }

    #[test]
    fn approval_prompt_neutralizes_names_and_arguments() {
        let args = serde_json::json!({ "path": "/tmp/x\u{1b}[8m hidden \u{1b}[0m", "n": 1 });
        let prompt = approval_prompt(Some("fs\u{1b}]0;title\u{7}"), "rm\r\u{1b}[2Kread", &args);
        assert!(!prompt.contains('\u{1b}') && !prompt.contains('\r') && !prompt.contains('\u{7}'));
        assert!(
            prompt.contains("Allow tool fs]0;title/rm[2Kread?"),
            "{prompt}"
        );
        // JSON keeps the escape visible as text rather than executing it.
        assert!(prompt.contains("\\u001b[8m hidden"), "{prompt}");
        let long = serde_json::json!({ "x": "a".repeat(5_000) });
        let prompt = approval_prompt(None, "t", &long);
        assert!(prompt.chars().count() < MAX_ARGS_PREVIEW + 150);
        // `{"x":"` + 5000 + `"}` = 5008 characters.
        assert!(
            prompt.contains("(3008 more characters not shown)"),
            "{prompt}"
        );
    }

    #[test]
    fn piped_output_is_unmodified() {
        assert_eq!(for_stream("a\x1b[1mb", false, true), "a\x1b[1mb");
        assert_eq!(for_stream("a\x1b[1mb", true, true), "a[1mb");
    }
}
