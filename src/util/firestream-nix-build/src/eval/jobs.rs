//! `nix-eval-jobs` stdout line schema and `Job` data type.

use std::collections::BTreeMap;

use serde::Deserialize;

/// Raw line emitted by `nix-eval-jobs`.
///
/// Per-line schema (one JSON object per stdout line):
/// ```jsonc
/// {
///   "attr": "packages.x86_64-linux.hello",
///   "drvPath": "/nix/store/...-hello.drv",
///   "outputs": { "out": "/nix/store/...-hello" },
///   "system": "x86_64-linux",
///   "isCached": false,                     // deprecated, kept for back-compat
///   "cacheStatus": "cached" | "local" | "unknown",
///   "error": null | "evaluation error message"
/// }
/// ```
#[derive(Debug, Deserialize, Clone)]
pub struct RawEvalLine {
    pub attr: Option<String>,
    #[serde(rename = "drvPath")]
    pub drv_path: Option<String>,
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
    pub system: Option<String>,
    #[serde(rename = "isCached", default)]
    pub is_cached: bool,
    #[serde(rename = "cacheStatus", default)]
    pub cache_status: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl RawEvalLine {
    /// True if the eval-jobs reported this attr as already substitutable from
    /// a binary cache. Prefers the newer `cacheStatus` field; falls back to
    /// the legacy `isCached`.
    pub fn is_cached_now(&self) -> bool {
        match self.cache_status.as_deref() {
            Some("cached") => true,
            Some(_) => false,
            None => self.is_cached,
        }
    }
}

/// An evaluated attribute ready to be built (or skipped if `--skip-cached`).
#[derive(Debug, Clone)]
pub struct Job {
    pub attr: String,
    pub drv_path: String,
    pub outputs: BTreeMap<String, String>,
    pub system: Option<String>,
}
