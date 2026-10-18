//! W3C traceparent parsing and encoding.
//!
//! Direct port of Go reference: `w3c/traceparent/traceparent.go`.

use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::str::FromStr;
use std::sync::OnceLock;

use rand::RngCore;
use regex::Regex;
use thiserror::Error;

/// Anchored only at the front per the W3C standard; traceparents can include
/// trailing fields but only the first four are required for our use.
fn traceparent_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^([[:xdigit:]]{2})-([[:xdigit:]]{32})-([[:xdigit:]]{16})-([[:xdigit:]]{2})")
            .expect("traceparent regex compiles")
    })
}

#[derive(Debug, Error)]
pub enum TraceparentParseError {
    #[error("could not parse invalid traceparent {0:?}")]
    Invalid(String),
    #[error("could not parse traceparent version in {0:?}")]
    Version(String),
    #[error("could not parse traceparent trace id in {0:?}")]
    TraceId(String),
    #[error("could not parse traceparent span id in {0:?}")]
    SpanId(String),
    #[error("could not parse traceparent flags in {0:?}")]
    Flags(String),
}

#[derive(Debug, Error)]
pub enum TraceparentIoError {
    #[error("could not open file {path:?} for read: {source}")]
    OpenRead {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not open file {path:?} for write: {source}")]
    OpenWrite {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("write error on file {path:?}: {source}")]
    Write {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("read error on file {path:?}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("file {path:?} was read but does not contain a valid traceparent")]
    NoneFound { path: String },
    #[error(transparent)]
    Parse(#[from] TraceparentParseError),
}

/// Parsed W3C traceparent.
///
/// `initialized` mirrors Go's flag indicating a successfully parsed struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Traceparent {
    pub version: u8,
    pub trace_id: [u8; 16],
    pub span_id: [u8; 8],
    pub flags: u8,
    pub initialized: bool,
}

impl Traceparent {
    /// True if the sampled flag (lowest bit) is set.
    pub fn is_sampled(&self) -> bool {
        self.flags & 0x01 != 0
    }

    /// Parse a traceparent string.
    ///
    /// Accepts the canonical W3C form `00-<trace-id>-<span-id>-<flags>` with
    /// optional `TRACEPARENT=` or `export TRACEPARENT=` shell-export prefix.
    pub fn parse(s: &str) -> Result<Self, TraceparentParseError> {
        let trimmed = s.trim();
        let stripped = trimmed
            .strip_prefix("export ")
            .unwrap_or(trimmed)
            .strip_prefix("TRACEPARENT=")
            .unwrap_or_else(|| trimmed.strip_prefix("export ").unwrap_or(trimmed));

        let caps = traceparent_re()
            .captures(stripped)
            .ok_or_else(|| TraceparentParseError::Invalid(s.to_string()))?;

        let version = u8::from_str_radix(&caps[1], 16)
            .map_err(|_| TraceparentParseError::Version(s.to_string()))?;

        let trace_vec = hex::decode(&caps[2])
            .map_err(|_| TraceparentParseError::TraceId(s.to_string()))?;
        if trace_vec.len() != 16 {
            return Err(TraceparentParseError::TraceId(s.to_string()));
        }
        let mut trace_id = [0u8; 16];
        trace_id.copy_from_slice(&trace_vec);

        let span_vec =
            hex::decode(&caps[3]).map_err(|_| TraceparentParseError::SpanId(s.to_string()))?;
        if span_vec.len() != 8 {
            return Err(TraceparentParseError::SpanId(s.to_string()));
        }
        let mut span_id = [0u8; 8];
        span_id.copy_from_slice(&span_vec);

        let flags = u8::from_str_radix(&caps[4], 16)
            .map_err(|_| TraceparentParseError::Flags(s.to_string()))?;

        Ok(Self {
            version,
            trace_id,
            span_id,
            flags,
            initialized: true,
        })
    }

    /// Encode in canonical `00-<32hex>-<16hex>-<2hex>` form.
    pub fn encode(&self) -> String {
        format!(
            "{:02x}-{}-{}-{:02x}",
            self.version,
            self.trace_id_string(),
            self.span_id_string(),
            self.flags
        )
    }

    pub fn trace_id_string(&self) -> String {
        hex::encode(self.trace_id)
    }

    pub fn span_id_string(&self) -> String {
        hex::encode(self.span_id)
    }

    /// Load from the `TRACEPARENT` env var. Returns `None` if unset/empty.
    pub fn from_env() -> Option<Result<Self, TraceparentParseError>> {
        match std::env::var("TRACEPARENT") {
            Ok(v) if !v.is_empty() => Some(Self::parse(&v)),
            _ => None,
        }
    }

    /// Read a traceparent from a file. The file may be a bare traceparent or a
    /// shell-export snippet (`export TRACEPARENT=...`). Comment lines starting
    /// with `#` are skipped.
    ///
    /// Returns an uninitialized `Traceparent` if no valid line is found
    /// (matches Go's silent-fail behaviour).
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, TraceparentIoError> {
        let path_ref = path.as_ref();
        let display = path_ref.display().to_string();
        let file =
            File::open(path_ref).map_err(|source| TraceparentIoError::OpenRead {
                path: display.clone(),
                source,
            })?;
        let reader = BufReader::new(file);

        let mut found: Option<String> = None;
        for line in reader.lines() {
            let line = line.map_err(|source| TraceparentIoError::Read {
                path: display.clone(),
                source,
            })?;
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                continue;
            }
            if trimmed.to_uppercase().contains("TRACEPARENT") {
                found = Some(trimmed.to_string());
                break;
            }
        }

        let line = match found {
            Some(s) => s,
            // silently return an uninitialized Traceparent (matches Go)
            None => return Ok(Self::default()),
        };

        let cleaned = line
            .strip_prefix("export ")
            .unwrap_or(&line)
            .strip_prefix("TRACEPARENT=")
            .unwrap_or_else(|| line.strip_prefix("export ").unwrap_or(&line));

        if !traceparent_re().is_match(cleaned) {
            return Err(TraceparentIoError::NoneFound { path: display });
        }

        Ok(Self::parse(cleaned)?)
    }

    /// Write this traceparent to a file in otel-cli's shell-compatible format.
    /// Prepends `export ` to the assignment if `export` is true.
    pub fn write_to_file<P: AsRef<Path>>(
        &self,
        path: P,
        export: bool,
    ) -> Result<(), TraceparentIoError> {
        let path_ref = path.as_ref();
        let display = path_ref.display().to_string();
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(path_ref)
            .map_err(|source| TraceparentIoError::OpenWrite {
                path: display.clone(),
                source,
            })?;

        let exported = if export { "export " } else { "" };
        let payload = format!(
            "# trace id: {}\n#  span id: {}\n{}TRACEPARENT={}\n",
            self.trace_id_string(),
            self.span_id_string(),
            exported,
            self.encode()
        );
        file.write_all(payload.as_bytes())
            .map_err(|source| TraceparentIoError::Write {
                path: display,
                source,
            })?;
        Ok(())
    }

    /// Generate a random traceparent with both ids freshly randomised and the
    /// sampled flag set. Use as the root span of a new trace.
    pub fn random() -> Self {
        let mut rng = rand::thread_rng();
        let mut trace_id = [0u8; 16];
        let mut span_id = [0u8; 8];
        rng.fill_bytes(&mut trace_id);
        rng.fill_bytes(&mut span_id);
        Self {
            version: 0,
            trace_id,
            span_id,
            flags: 0x01,
            initialized: true,
        }
    }

    /// Derive a child traceparent: same `trace_id`, fresh random `span_id`,
    /// flags preserved.
    pub fn new_child(&self) -> Self {
        let mut rng = rand::thread_rng();
        let mut span_id = [0u8; 8];
        rng.fill_bytes(&mut span_id);
        Self {
            version: self.version,
            trace_id: self.trace_id,
            span_id,
            flags: self.flags,
            initialized: true,
        }
    }
}

impl fmt::Display for Traceparent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.encode())
    }
}

impl FromStr for Traceparent {
    type Err = TraceparentParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    const SAMPLE: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";

    #[test]
    fn parse_standard() {
        let tp = Traceparent::parse(SAMPLE).expect("parses");
        assert_eq!(tp.version, 0);
        assert_eq!(tp.trace_id_string(), "0af7651916cd43dd8448eb211c80319c");
        assert_eq!(tp.span_id_string(), "b7ad6b7169203331");
        assert_eq!(tp.flags, 0x01);
        assert!(tp.initialized);
    }

    #[test]
    fn parse_with_export_prefix() {
        let s = format!("export TRACEPARENT={SAMPLE}");
        let tp = Traceparent::parse(&s).expect("parses");
        assert_eq!(tp.encode(), SAMPLE);
    }

    #[test]
    fn parse_with_equals_prefix() {
        let s = format!("TRACEPARENT={SAMPLE}");
        let tp = Traceparent::parse(&s).expect("parses");
        assert_eq!(tp.encode(), SAMPLE);
    }

    #[test]
    fn parse_invalid_returns_err() {
        let err = Traceparent::parse("not a traceparent");
        assert!(err.is_err());
        let err = Traceparent::parse("00-tooShort-0-01");
        assert!(err.is_err());
    }

    #[test]
    fn encode_matches_input() {
        let tp = Traceparent::parse(SAMPLE).expect("parses");
        assert_eq!(tp.encode(), SAMPLE);
    }

    #[test]
    fn encode_zero_traceparent() {
        let tp = Traceparent::default();
        assert_eq!(
            tp.encode(),
            "00-00000000000000000000000000000000-0000000000000000-00"
        );
    }

    #[test]
    fn is_sampled_flag() {
        let mut tp = Traceparent {
            flags: 0x01,
            ..Default::default()
        };
        assert!(tp.is_sampled());
        tp.flags = 0x00;
        assert!(!tp.is_sampled());
    }

    #[test]
    fn new_child_keeps_trace_id() {
        let parent = Traceparent::random();
        let child = parent.new_child();
        assert_eq!(parent.trace_id, child.trace_id);
        assert_ne!(parent.span_id, child.span_id);
    }

    #[test]
    fn random_is_valid_hex() {
        let tp = Traceparent::random();
        // round-trip through parse to validate the encoded form is well-formed
        let parsed = Traceparent::parse(&tp.encode()).expect("round-trip parses");
        assert_eq!(parsed.trace_id, tp.trace_id);
        assert_eq!(parsed.span_id, tp.span_id);
        assert!(tp.is_sampled());
    }

    #[test]
    fn display_uses_encode() {
        let tp = Traceparent::parse(SAMPLE).expect("parses");
        assert_eq!(format!("{tp}"), SAMPLE);
    }

    #[test]
    fn from_str_works() {
        let tp: Traceparent = SAMPLE.parse().expect("parses");
        assert_eq!(tp.encode(), SAMPLE);
    }

    #[test]
    fn write_then_read_file_roundtrip() {
        let tp = Traceparent::parse(SAMPLE).expect("parses");
        let tmp = NamedTempFile::new().expect("tempfile");
        tp.write_to_file(tmp.path(), false).expect("write");

        let loaded = Traceparent::from_file(tmp.path()).expect("load");
        assert_eq!(loaded.encode(), SAMPLE);
    }

    #[test]
    fn write_with_export_includes_export_keyword() {
        let tp = Traceparent::parse(SAMPLE).expect("parses");
        let tmp = NamedTempFile::new().expect("tempfile");
        tp.write_to_file(tmp.path(), true).expect("write");

        let bytes = std::fs::read_to_string(tmp.path()).expect("read");
        assert!(bytes.contains("export TRACEPARENT="));
    }

    #[test]
    fn from_file_finds_traceparent_amid_comments() {
        let mut tmp = NamedTempFile::new().expect("tempfile");
        writeln!(tmp, "# this is a comment").unwrap();
        writeln!(tmp, "# trace id: deadbeef").unwrap();
        writeln!(tmp, "export TRACEPARENT={SAMPLE}").unwrap();
        tmp.flush().unwrap();

        let loaded = Traceparent::from_file(tmp.path()).expect("load");
        assert_eq!(loaded.encode(), SAMPLE);
    }

    #[test]
    fn from_file_returns_default_when_no_traceparent_found() {
        let tmp = NamedTempFile::new().expect("tempfile");
        // empty file, no TRACEPARENT line: Go silently returns zero value
        let loaded = Traceparent::from_file(tmp.path()).expect("load");
        assert!(!loaded.initialized);
    }
}
