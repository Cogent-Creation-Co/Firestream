//! Per-phase outcome records and serialization (JSON / JUnit).

use std::collections::BTreeMap;
use std::io::Write;

use anyhow::{Context, Result};
use serde::Serialize;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ResultKind {
    Eval,
    Build,
    Upload,
    Download,
    Cachix,
    Attic,
    Niks3,
}

impl ResultKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ResultKind::Eval => "EVAL",
            ResultKind::Build => "BUILD",
            ResultKind::Upload => "UPLOAD",
            ResultKind::Download => "DOWNLOAD",
            ResultKind::Cachix => "CACHIX",
            ResultKind::Attic => "ATTIC",
            ResultKind::Niks3 => "NIKS3",
        }
    }

    /// "Eval", "Build", "Upload", ... — used for JUnit classname.
    pub fn title_case(self) -> &'static str {
        match self {
            ResultKind::Eval => "Eval",
            ResultKind::Build => "Build",
            ResultKind::Upload => "Upload",
            ResultKind::Download => "Download",
            ResultKind::Cachix => "Cachix",
            ResultKind::Attic => "Attic",
            ResultKind::Niks3 => "Niks3",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub kind: ResultKind,
    pub attr: String,
    pub success: bool,
    pub duration: f64,
    pub error: Option<String>,
    pub log_output: Option<String>,
    pub outputs: Option<BTreeMap<String, String>>,
}

impl Outcome {
    pub fn eval_ok(attr: impl Into<String>, duration: f64) -> Self {
        Self {
            kind: ResultKind::Eval,
            attr: attr.into(),
            success: true,
            duration,
            error: None,
            log_output: None,
            outputs: None,
        }
    }

    pub fn eval_err(attr: impl Into<String>, error: impl Into<String>, duration: f64) -> Self {
        Self {
            kind: ResultKind::Eval,
            attr: attr.into(),
            success: false,
            duration,
            error: Some(error.into()),
            log_output: None,
            outputs: None,
        }
    }
}

/// Mirrors the Python `dump_json()` shape:
/// `{"results": [{"type", "attr", "success", "duration", "error", ?"outputs"}]}`
/// with `indent=2, sort_keys=True`.
#[derive(Serialize)]
struct JsonOutcome<'a> {
    attr: &'a str,
    duration: f64,
    error: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outputs: Option<&'a BTreeMap<String, String>>,
    success: bool,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct JsonResults<'a> {
    results: Vec<JsonOutcome<'a>>,
}

pub fn dump_json<W: Write>(mut w: W, outcomes: &[Outcome]) -> Result<()> {
    let payload = JsonResults {
        results: outcomes
            .iter()
            .map(|o| JsonOutcome {
                attr: &o.attr,
                duration: o.duration,
                error: o.error.as_deref(),
                outputs: o.outputs.as_ref(),
                success: o.success,
                kind: o.kind.as_str(),
            })
            .collect(),
    };
    // serde_json's pretty printer uses 2-space indent (matches Python's
    // `indent=2`); JsonOutcome fields are declared in alphabetical order to
    // mimic `sort_keys=True`.
    let s = serde_json::to_string_pretty(&payload).context("serialize results to JSON")?;
    w.write_all(s.as_bytes()).context("write result JSON")?;
    Ok(())
}

/// Mirrors `dump_junit_xml()` at `__init__.py:1923-1965`.
pub fn dump_junit_xml<W: Write>(mut w: W, suite_name: &str, outcomes: &[Outcome]) -> Result<()> {
    let failures = outcomes.iter().filter(|r| !r.success).count();
    let mut s = String::new();
    s.push_str("<testsuites>");
    s.push_str(&format!(
        "<testsuite name=\"{}\" tests=\"{}\" failures=\"{}\">",
        xml_escape(suite_name),
        outcomes.len(),
        failures
    ));
    for r in outcomes {
        s.push_str(&format!(
            "<testcase classname=\"{}\" name=\"{}\" time=\"{}\">",
            r.kind.title_case(),
            xml_escape(&r.attr),
            r.duration
        ));
        if !r.success {
            let msg = r.error.as_deref().unwrap_or("<no message>");
            s.push_str(&format!(
                "<failure message=\"{}\" type=\"BuildFailure\">{}</failure>",
                xml_escape(msg),
                xml_escape(r.error.as_deref().unwrap_or(""))
            ));
        }
        s.push_str("</testcase>");
    }
    s.push_str("</testsuite>");
    s.push_str("</testsuites>");
    w.write_all(s.as_bytes()).context("write JUnit XML")?;
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
