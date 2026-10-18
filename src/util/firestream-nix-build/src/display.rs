//! Human-readable presentation helpers for stderr lines that flow into the
//! live dashboard's per-task [`LineRing`].
//!
//! When `nix-fast-build` is run with `--log-format internal-json -v` (the path
//! that feeds the in-process OTel ingest), every stderr line is one of:
//!
//! - a raw `@nix {...}` JSON record (build progress, activity start/stop,
//!   structured messages);
//! - a plain line nix wrote before/around the JSON stream (rare).
//!
//! Both are fine for the OTel pipeline (it parses them) and for the on-disk
//! `stderr_log` (it wants the raw bytes for postmortem). But the dashboard
//! tail preview shows the *user*, and they don't want to read
//! `@nix {"action":"result","fields":["[1m[92m  Compiling …"]}`.
//!
//! [`RingFormatter::humanize`] is the single seam: feed it a stderr line, get
//! back the string to push into the ring (or `None` to suppress entirely).

use otel_cli::ingest::nix_internal_json::{
    self as njson, ACT_BUILD, NIX_PREFIX, NixEvent, Parser, RESULT_BUILD_LOG_LINE,
};

/// Per-build humanizer. Owns a `Parser` so drift counters accumulate
/// per-task; cheap to allocate.
#[derive(Debug, Default)]
pub struct RingFormatter {
    parser: Parser,
}

impl RingFormatter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide what (if anything) to push into the per-task ring for `line`.
    ///
    /// - `@nix` build-log results → the underlying log text, ANSI-stripped.
    /// - `@nix` build activity starts → `▶ building <drv-short-name>`.
    /// - `@nix` free-standing messages at info or louder → the message text.
    /// - `@nix` activity stops, progress counters, debug noise → suppressed.
    /// - Non-`@nix` lines → ANSI-stripped passthrough.
    pub fn humanize(&mut self, line: String) -> Option<String> {
        if !line.trim_start().starts_with(NIX_PREFIX) {
            let stripped = strip_ansi(&line);
            if stripped.trim().is_empty() {
                return None;
            }
            return Some(stripped);
        }

        match self.parser.parse_line(&line) {
            NixEvent::Result { kind, fields, .. } if kind == RESULT_BUILD_LOG_LINE => {
                let text = fields.first().and_then(|v| v.as_str())?;
                let stripped = strip_ansi(text);
                if stripped.trim().is_empty() {
                    None
                } else {
                    Some(stripped)
                }
            }
            NixEvent::ActivityStart {
                kind, text, fields, ..
            } if kind == ACT_BUILD => {
                let drv = njson::extract_drv_path(&fields, &text)?;
                let short = njson::drv_short_name(&drv);
                Some(format!("▶ building {short}"))
            }
            NixEvent::Message { level, text } if level <= 3 => {
                let stripped = strip_ansi(&text);
                if stripped.trim().is_empty() {
                    None
                } else {
                    Some(stripped)
                }
            }
            NixEvent::Unknown { raw } if !raw.is_empty() => Some(raw),
            // Suppressed: numeric progress results, ActivityStop, low-level
            // Message, empty Unknown, non-build activity starts (substitute /
            // copy-path are loud and not useful in the tail).
            _ => None,
        }
    }
}

/// Strip ANSI CSI (`\x1b[…`) and OSC (`\x1b]…`) escape sequences from `s`.
///
/// Build tools like cargo emit color codes by default; the dashboard renders
/// to a styled-text widget and we don't want raw escape codes leaking into
/// the rendered tail.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            match chars.peek() {
                Some(&'[') => {
                    chars.next();
                    while let Some(&nc) = chars.peek() {
                        chars.next();
                        if nc.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                Some(&']') => {
                    chars.next();
                    while let Some(&nc) = chars.peek() {
                        chars.next();
                        if nc == '\x07' {
                            break;
                        }
                        if nc == '\x1b' && matches!(chars.peek(), Some(&'\\')) {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_ansi_removes_csi_and_osc() {
        assert_eq!(strip_ansi("\x1b[1m\x1b[92m Compiling\x1b[0m foo"), " Compiling foo");
        assert_eq!(
            strip_ansi("title \x1b]0;hello\x07after"),
            "title after"
        );
        assert_eq!(strip_ansi("no escapes here"), "no escapes here");
    }

    #[test]
    fn humanize_unwraps_at_nix_result_log_line() {
        let mut f = RingFormatter::new();
        // Nix wire-encodes ESC as the six characters `\u001b` in the JSON
        // payload (a valid JSON unicode escape); serde_json decodes that back
        // to a real ESC byte before our strip_ansi sees it.
        let line = "@nix {\"action\":\"result\",\"fields\":[\"\\u001b[1m\\u001b[92m   Compiling tokio v1.45.0\\u001b[0m\"],\"id\":7,\"type\":101}";
        let out = f.humanize(line.to_string()).expect("kept");
        assert_eq!(out, "   Compiling tokio v1.45.0");
    }

    #[test]
    fn humanize_suppresses_numeric_progress_results() {
        let mut f = RingFormatter::new();
        // type=105 with numeric fields is a build-activity progress counter,
        // not a log line — nothing useful to render.
        assert!(
            f.humanize(r#"@nix {"action":"result","fields":[0,0,0,0],"id":1,"type":105}"#.to_string())
                .is_none()
        );
        assert!(
            f.humanize(r#"@nix {"action":"result","fields":[101,0],"id":1,"type":106}"#.to_string())
                .is_none()
        );
    }

    #[test]
    fn humanize_summarises_build_activity_start() {
        let mut f = RingFormatter::new();
        let line = r#"@nix {"action":"start","fields":["/nix/store/2rrl32mmyy7aljmxfh3lni6kqidma1jl-otel-ingest-test3.drv","",1,1],"id":42,"level":3,"parent":0,"text":"building '/nix/store/2rrl32mmyy7aljmxfh3lni6kqidma1jl-otel-ingest-test3.drv'","type":105}"#;
        assert_eq!(
            f.humanize(line.to_string()).as_deref(),
            Some("▶ building otel-ingest-test3")
        );
    }

    #[test]
    fn humanize_suppresses_activity_stop() {
        let mut f = RingFormatter::new();
        assert!(
            f.humanize(r#"@nix {"action":"stop","id":1}"#.to_string())
                .is_none()
        );
    }

    #[test]
    fn humanize_plain_line_strips_ansi() {
        let mut f = RingFormatter::new();
        assert_eq!(
            f.humanize("\x1b[31merror:\x1b[0m build failed".to_string()).as_deref(),
            Some("error: build failed")
        );
    }

    #[test]
    fn humanize_malformed_at_nix_passes_through_raw() {
        let mut f = RingFormatter::new();
        // Torn write: not valid JSON. The parser returns Unknown { raw }; we
        // keep it so the user can still see *something* hit stderr.
        let line = r#"@nix {"action":"start","id":1"#;
        let out = f.humanize(line.to_string()).expect("kept");
        assert!(out.contains("@nix"));
    }

    #[test]
    fn humanize_drops_blank_lines() {
        let mut f = RingFormatter::new();
        assert!(f.humanize(String::new()).is_none());
        assert!(f.humanize("   \t".to_string()).is_none());
    }
}
