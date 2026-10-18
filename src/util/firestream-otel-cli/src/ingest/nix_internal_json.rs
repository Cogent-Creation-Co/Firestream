//! Nix `internal-json` schema adapter (PRD §9.4, §13).
//!
//! `nix … --log-format internal-json` emits one record per line, each prefixed
//! with the literal `@nix ` followed by a JSON object. This module is the *only*
//! place that knows the wire shape — everything downstream consumes the stable
//! [`NixEvent`] enum. When Nix changes its `internal-json` schema, the blast
//! radius is contained here: unrecognised records degrade to
//! [`NixEvent::Unknown`] (counted, never fatal) rather than crashing the
//! pipeline.
//!
//! ### Observed record shapes (Nix 2.x)
//!
//! ```text
//! @nix {"action":"start","id":<u64>,"level":<int>,"parent":<u64>,"text":"...","type":<int>,"fields":[...]}
//! @nix {"action":"stop","id":<u64>}
//! @nix {"action":"result","id":<u64>,"type":<int>,"fields":[...]}
//! @nix {"action":"msg","level":<int>,"msg":"..."}
//! ```
//!
//! Build activities carry `type` [`ACT_BUILD`] (105) and a `text` of the form
//! `building '/nix/store/<hash>-<name>.drv'`; `fields[0]` is the same `.drv`
//! path. Result records of `type` [`RESULT_BUILD_LOG_LINE`] (101) carry a single
//! log-line string in `fields[0]`.

use std::collections::BTreeSet;

use serde::Deserialize;

/// Line prefix Nix prepends to every `internal-json` record.
pub const NIX_PREFIX: &str = "@nix ";

/// Activity `type` for a derivation build (`ActBuild` in Nix's `logging.hh`).
/// The activity's `text` is `building '<drv>'` and `fields[0]` is the `.drv`
/// path.
pub const ACT_BUILD: i64 = 105;

/// Activity `type` for a substitution (binary-cache fetch). We model these as
/// spans too so cache misses are visible, but they are lower priority than
/// builds for the `--min-activity-level` gate.
pub const ACT_SUBSTITUTE: i64 = 100;

/// Activity `type` for copying a path (`ActCopyPath`).
pub const ACT_COPY_PATH: i64 = 100;

/// Result `type` for a single build-log line (`ResBuildLogLine` in Nix). The
/// log text is `fields[0]`.
pub const RESULT_BUILD_LOG_LINE: i64 = 101;

/// Stable, version-drift-isolated event model. Everything past this boundary
/// works against this enum, never the raw JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum NixEvent {
    /// An activity began. `id` is reused after the matching `Stop`.
    ActivityStart {
        id: u64,
        parent: u64,
        kind: i64,
        text: String,
        fields: Vec<serde_json::Value>,
    },
    /// An activity finished.
    ActivityStop { id: u64 },
    /// A progress/result record attached to an activity (e.g. a log line).
    Result {
        id: u64,
        kind: i64,
        fields: Vec<serde_json::Value>,
    },
    /// A free-standing log message not tied to a specific activity.
    Message { level: i64, text: String },
    /// An unparseable or unrecognised line. Carries the raw text for
    /// debugging. Counted via [`Parser::unknown_records`]; never fatal.
    Unknown { raw: String },
}

/// Raw record shape, deserialized by serde then mapped into [`NixEvent`]. Kept
/// private so the unstable field set never leaks past this module.
#[derive(Debug, Deserialize)]
struct RawRecord {
    action: String,
    #[serde(default)]
    id: u64,
    #[serde(default)]
    parent: u64,
    #[serde(default)]
    level: i64,
    #[serde(default, rename = "type")]
    kind: i64,
    #[serde(default)]
    text: String,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    fields: Vec<serde_json::Value>,
}

/// Streaming parser. Holds the drift-observability counters so the ingest loop
/// can attach them to the root span at end-of-stream.
#[derive(Debug, Default)]
pub struct Parser {
    /// Count of lines that did not parse into a known shape (PRD §13).
    pub unknown_records: u64,
    /// Set of `action` strings observed across the stream.
    pub actions_observed: BTreeSet<String>,
    /// Set of activity/result `type` values observed.
    pub types_observed: BTreeSet<i64>,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse a single raw line (with or without the `@nix ` prefix, with or
    /// without a trailing newline) into a [`NixEvent`]. Updates the observed
    /// counters as a side effect. Never returns `Err`: malformed input becomes
    /// [`NixEvent::Unknown`] so a single bad line can't abort ingestion.
    pub fn parse_line(&mut self, line: &str) -> NixEvent {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let trimmed = trimmed.trim();
        if trimmed.is_empty() {
            // Empty line — not an unknown record, just nothing.
            return NixEvent::Unknown {
                raw: String::new(),
            };
        }

        // The prefix is the wire convention; tolerate its absence so callers can
        // feed pre-stripped lines (and so we don't break if Nix drops it).
        let json_part = trimmed.strip_prefix(NIX_PREFIX).unwrap_or(trimmed);

        let raw: RawRecord = match serde_json::from_str(json_part) {
            Ok(r) => r,
            Err(_) => {
                self.unknown_records += 1;
                return NixEvent::Unknown {
                    raw: trimmed.to_string(),
                };
            }
        };

        self.actions_observed.insert(raw.action.clone());
        if matches!(raw.action.as_str(), "start" | "result") {
            self.types_observed.insert(raw.kind);
        }

        match raw.action.as_str() {
            "start" => NixEvent::ActivityStart {
                id: raw.id,
                parent: raw.parent,
                kind: raw.kind,
                text: raw.text,
                fields: raw.fields,
            },
            "stop" => NixEvent::ActivityStop { id: raw.id },
            "result" => NixEvent::Result {
                id: raw.id,
                kind: raw.kind,
                fields: raw.fields,
            },
            "msg" => NixEvent::Message {
                level: raw.level,
                text: raw.msg,
            },
            // A known JSON object with an action we don't model. Still counted
            // as unknown so schema additions are loud, but we keep the raw text.
            _ => {
                self.unknown_records += 1;
                NixEvent::Unknown {
                    raw: trimmed.to_string(),
                }
            }
        }
    }

    /// Comma-joined sorted set of observed `action`+`type` tokens, for the
    /// `nix.internal_json.fields_observed` root-span attribute (PRD §13). The
    /// shape is `action:start,action:stop,type:101,type:105` — stable across
    /// runs so drift shows up as a diff.
    pub fn fields_observed(&self) -> String {
        let mut tokens: Vec<String> = self
            .actions_observed
            .iter()
            .map(|a| format!("action:{a}"))
            .collect();
        for t in &self.types_observed {
            tokens.push(format!("type:{t}"));
        }
        tokens.join(",")
    }
}

/// Extract the `.drv` store path from a build activity. Prefers `fields[0]`
/// (the structured form Nix 2.x emits: `["<drv>","",<u>,<u>]`), falling back to
/// the `building '<drv>'` text. Returns `None` if neither yields a `.drv` path.
pub fn extract_drv_path(fields: &[serde_json::Value], text: &str) -> Option<String> {
    // Structured field form: fields[0] is the .drv path string.
    if let Some(serde_json::Value::String(s)) = fields.first() {
        if s.ends_with(".drv") && s.starts_with("/nix/store/") {
            return Some(s.clone());
        }
    }
    // Text form: building '/nix/store/<hash>-<name>.drv'
    drv_from_text(text)
}

/// Pull a `.drv` path out of a free-form activity text like
/// `building '/nix/store/<hash>-<name>.drv'`. Quote-delimited and bare forms
/// are both handled.
fn drv_from_text(text: &str) -> Option<String> {
    // Find a token that looks like a store .drv path.
    for token in text.split(['\'', '"', ' ']) {
        if token.starts_with("/nix/store/") && token.ends_with(".drv") {
            return Some(token.to_string());
        }
    }
    None
}

/// Derive a human span name from a `.drv` path. `/nix/store/<hash>-foo.drv`
/// becomes `foo` (hash + `.drv` stripped). Used to build `nix.build.<name>`.
pub fn drv_short_name(drv_path: &str) -> String {
    let base = drv_path
        .rsplit('/')
        .next()
        .unwrap_or(drv_path)
        .trim_end_matches(".drv");
    // Strip the leading `<32hash>-` if present.
    match base.split_once('-') {
        Some((hash, rest)) if hash.len() >= 16 && hash.bytes().all(|b| b.is_ascii_alphanumeric()) => {
            rest.to_string()
        }
        _ => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A real build-activity start line captured from `nix-build … --log-format
    // internal-json -v` (Nix 2.34).
    const BUILD_START: &str = r#"@nix {"action":"start","fields":["/nix/store/2rrl32mmyy7aljmxfh3lni6kqidma1jl-otel-ingest-test3.drv","",1,1],"id":6394394554925063,"level":3,"parent":0,"text":"building '/nix/store/2rrl32mmyy7aljmxfh3lni6kqidma1jl-otel-ingest-test3.drv'","type":105}"#;

    #[test]
    fn parses_build_start_and_extracts_drv() {
        let mut p = Parser::new();
        let ev = p.parse_line(BUILD_START);
        match ev {
            NixEvent::ActivityStart {
                id,
                parent,
                kind,
                ref text,
                ref fields,
            } => {
                assert_eq!(id, 6394394554925063);
                assert_eq!(parent, 0);
                assert_eq!(kind, ACT_BUILD);
                assert!(text.starts_with("building '"));
                let drv = extract_drv_path(fields, text).expect("drv extracted");
                assert_eq!(
                    drv,
                    "/nix/store/2rrl32mmyy7aljmxfh3lni6kqidma1jl-otel-ingest-test3.drv"
                );
            }
            other => panic!("expected ActivityStart, got {other:?}"),
        }
    }

    #[test]
    fn extract_drv_falls_back_to_text() {
        // Empty fields → must recover the .drv from the text form.
        let fields: Vec<serde_json::Value> = vec![];
        let drv = extract_drv_path(
            &fields,
            "building '/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-pkg.drv'",
        )
        .unwrap();
        assert_eq!(
            drv,
            "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-pkg.drv"
        );
    }

    #[test]
    fn drv_short_name_strips_hash_and_suffix() {
        let n = drv_short_name("/nix/store/2rrl32mmyy7aljmxfh3lni6kqidma1jl-otel-ingest-test3.drv");
        assert_eq!(n, "otel-ingest-test3");
    }

    #[test]
    fn parses_stop() {
        let mut p = Parser::new();
        let ev = p.parse_line(r#"@nix {"action":"stop","id":1}"#);
        assert_eq!(ev, NixEvent::ActivityStop { id: 1 });
    }

    #[test]
    fn parses_result_log_line() {
        let mut p = Parser::new();
        let ev = p.parse_line(r#"@nix {"action":"result","fields":["phase1"],"id":7,"type":101}"#);
        match ev {
            NixEvent::Result { id, kind, fields } => {
                assert_eq!(id, 7);
                assert_eq!(kind, RESULT_BUILD_LOG_LINE);
                assert_eq!(fields[0], serde_json::Value::String("phase1".to_string()));
            }
            other => panic!("expected Result, got {other:?}"),
        }
    }

    #[test]
    fn parses_msg() {
        let mut p = Parser::new();
        let ev = p.parse_line(r#"@nix {"action":"msg","level":3,"msg":"this derivation will be built:"}"#);
        assert_eq!(
            ev,
            NixEvent::Message {
                level: 3,
                text: "this derivation will be built:".to_string()
            }
        );
    }

    #[test]
    fn garbage_line_is_unknown_and_counted_non_fatal() {
        let mut p = Parser::new();
        let ev = p.parse_line("this is not json at all");
        assert!(matches!(ev, NixEvent::Unknown { .. }));
        assert_eq!(p.unknown_records, 1);

        // A well-formed JSON object with an unmodelled action is also unknown.
        let ev2 = p.parse_line(r#"@nix {"action":"futureThing","id":99}"#);
        assert!(matches!(ev2, NixEvent::Unknown { .. }));
        assert_eq!(p.unknown_records, 2, "both bad lines counted");

        // Truncated JSON (torn write) — must not panic, just count.
        let ev3 = p.parse_line(r#"@nix {"action":"start","id":1"#);
        assert!(matches!(ev3, NixEvent::Unknown { .. }));
        assert_eq!(p.unknown_records, 3);
    }

    #[test]
    fn empty_line_is_not_counted() {
        let mut p = Parser::new();
        let _ = p.parse_line("");
        let _ = p.parse_line("   \n");
        assert_eq!(p.unknown_records, 0, "blank lines are not drift");
    }

    #[test]
    fn fields_observed_is_stable_and_sorted() {
        let mut p = Parser::new();
        p.parse_line(BUILD_START);
        p.parse_line(r#"@nix {"action":"stop","id":1}"#);
        p.parse_line(r#"@nix {"action":"result","fields":["x"],"id":1,"type":101}"#);
        let obs = p.fields_observed();
        // actions sorted then types sorted.
        assert_eq!(obs, "action:result,action:start,action:stop,type:101,type:105");
    }

    #[test]
    fn tolerates_missing_prefix() {
        let mut p = Parser::new();
        let ev = p.parse_line(r#"{"action":"stop","id":42}"#);
        assert_eq!(ev, NixEvent::ActivityStop { id: 42 });
    }
}
