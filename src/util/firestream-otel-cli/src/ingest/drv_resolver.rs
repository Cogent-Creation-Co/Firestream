//! Per-derivation enrichment via `nix` CLI calls (PRD §9.4).
//!
//! `internal-json` gives us span lifecycle but not the `.drv` metadata or build
//! log. At span close we shell out:
//!
//! - `nix derivation show <drv>` → system, output names/paths, drv name. (JSON
//!   is the default output; a trailing `--json` is rejected on Nix 2.x.) Cached
//!   per `.drv` (the same derivation may close more than once across a build
//!   set, and `nix derivation show` is not free).
//! - `nix log <drv>` → total byte count always; the last 4 KiB *only* on
//!   failure (so the trace stays small but failures are debuggable). Requires
//!   `keep-failed = true` in nix.conf (W8) for post-failure logs to exist.
//!
//! All calls are best-effort: a missing/older `nix`, a gc'd drv, or a permission
//! error leaves the corresponding attributes unset rather than failing the
//! ingest.

use std::collections::HashMap;

use serde::Deserialize;
use tokio::process::Command;

/// Maximum bytes of build log retained as `build.log.tail` on failure.
pub const LOG_TAIL_BYTES: usize = 4 * 1024;

/// Structured `nix derivation show` output for one derivation.
#[derive(Debug, Clone, Default)]
pub struct DrvInfo {
    pub name: String,
    pub system: String,
    /// Output paths, joined `out=/nix/store/...,dev=/nix/store/...` for the
    /// `nix.derivation.outputs` attribute.
    pub outputs: String,
}

/// Captured build log for a derivation.
#[derive(Debug, Clone, Default)]
pub struct LogInfo {
    /// Total bytes in `nix log` output.
    pub bytes: u64,
    /// Last [`LOG_TAIL_BYTES`] of the log — populated only when `on_failure`.
    pub tail: Option<String>,
}

/// In-process resolver with a `nix derivation show` cache. One per ingest run.
#[derive(Debug, Default)]
pub struct DrvResolver {
    cache: HashMap<String, Option<DrvInfo>>,
    /// Count of cgroup probes that found nothing — surfaced on the root span as
    /// `nix.cgroup.probe_missing` (PRD §10.1). Lives here so the ingest loop has
    /// one place to bump it (the resolver owns the per-drv enrichment pass).
    pub cgroup_probe_missing: u64,
}

impl DrvResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve `nix derivation show <drv> --json`, cached. Returns `None` when
    /// the call fails or yields nothing usable (older nix, gc'd drv, …).
    pub async fn derivation_info(&mut self, drv_path: &str) -> Option<DrvInfo> {
        if let Some(cached) = self.cache.get(drv_path) {
            return cached.clone();
        }
        let info = query_derivation(drv_path).await;
        self.cache.insert(drv_path.to_string(), info.clone());
        info
    }

    /// Resolve the build log for `drv_path`. `bytes` is always set; `tail` is
    /// populated only when `on_failure` (PRD §9.4 keeps the trace small on the
    /// happy path). Not cached — logs grow during the build and a closing span
    /// wants the final size.
    pub async fn build_log(&mut self, drv_path: &str, on_failure: bool) -> Option<LogInfo> {
        query_log(drv_path, on_failure).await
    }
}

/// Top-level shape of `nix derivation show --json`:
/// `{"derivations": {"<basename>.drv": {...}}, "version": <int>}` on newer nix,
/// or the legacy bare `{"<path>.drv": {...}}` map on older nix. We accept both.
#[derive(Debug, Deserialize)]
struct DerivationShowEnvelope {
    #[serde(default)]
    derivations: HashMap<String, RawDrv>,
}

#[derive(Debug, Deserialize)]
struct RawDrv {
    #[serde(default)]
    name: String,
    #[serde(default)]
    system: String,
    #[serde(default)]
    outputs: HashMap<String, RawOutput>,
}

#[derive(Debug, Deserialize)]
struct RawOutput {
    #[serde(default)]
    path: String,
}

async fn query_derivation(drv_path: &str) -> Option<DrvInfo> {
    // `nix derivation show` emits JSON by default; a trailing `--json` is
    // *rejected* on Nix 2.x (the flag isn't valid in that position). So we omit
    // it and rely on the default. The `--extra-experimental-features` guard
    // keeps this working when the caller's environment hasn't enabled the
    // unified `nix` command globally.
    let out = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "derivation",
            "show",
            drv_path,
        ])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_derivation_show(&out.stdout, drv_path)
}

/// Parse `nix derivation show` JSON. Factored out for unit testing without a
/// live `nix`. Handles both the `{"derivations": {...}}` envelope (nix ≥ 2.x
/// recent) and the legacy bare-map form.
fn parse_derivation_show(stdout: &[u8], drv_path: &str) -> Option<DrvInfo> {
    // Try the enveloped form first.
    if let Ok(env) = serde_json::from_slice::<DerivationShowEnvelope>(stdout) {
        if let Some(raw) = pick_drv(&env.derivations, drv_path) {
            return Some(to_info(raw));
        }
    }
    // Legacy bare map: `{"<path>": {...}}`.
    if let Ok(map) = serde_json::from_slice::<HashMap<String, RawDrv>>(stdout) {
        if let Some(raw) = pick_drv(&map, drv_path) {
            return Some(to_info(raw));
        }
    }
    None
}

/// Pick the matching derivation from the keyed map. The key may be the full
/// `.drv` path or just its basename depending on nix version; match on suffix.
fn pick_drv<'a>(map: &'a HashMap<String, RawDrv>, drv_path: &str) -> Option<&'a RawDrv> {
    let basename = drv_path.rsplit('/').next().unwrap_or(drv_path);
    map.iter()
        .find(|(k, _)| k.as_str() == drv_path || k.ends_with(basename))
        .map(|(_, v)| v)
        // Single-entry maps: just take the only value.
        .or_else(|| if map.len() == 1 { map.values().next() } else { None })
}

fn to_info(raw: &RawDrv) -> DrvInfo {
    // Deterministic ordering for the joined outputs attribute.
    let mut outs: Vec<(String, String)> = raw
        .outputs
        .iter()
        .map(|(k, v)| (k.clone(), normalize_store_path(&v.path)))
        .collect();
    outs.sort();
    let outputs = outs
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(",");
    DrvInfo {
        name: raw.name.clone(),
        system: raw.system.clone(),
        outputs,
    }
}

/// `nix derivation show` reports output paths without the `/nix/store/` prefix
/// in the `outputs.<name>.path` field on some versions and with it on others.
/// Normalize to the full store path for the span attribute.
fn normalize_store_path(p: &str) -> String {
    if p.is_empty() || p.starts_with("/nix/store/") {
        p.to_string()
    } else {
        format!("/nix/store/{p}")
    }
}

async fn query_log(drv_path: &str, on_failure: bool) -> Option<LogInfo> {
    let out = Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command",
            "log",
            drv_path,
        ])
        .output()
        .await
        .ok()?;
    // `nix log` prints to stdout. A missing log → empty stdout / failure; treat
    // as "no log info" rather than a span attribute of 0 we'd have to explain.
    if !out.status.success() && out.stdout.is_empty() {
        return None;
    }
    Some(log_info_from_bytes(&out.stdout, on_failure))
}

/// Build a [`LogInfo`] from raw log bytes. Tail is the last [`LOG_TAIL_BYTES`]
/// decoded lossily as UTF-8, taken on a char boundary so we never split a
/// multibyte sequence. Factored out for unit testing.
fn log_info_from_bytes(stdout: &[u8], on_failure: bool) -> LogInfo {
    let bytes = stdout.len() as u64;
    let tail = if on_failure {
        let start = stdout.len().saturating_sub(LOG_TAIL_BYTES);
        Some(String::from_utf8_lossy(&stdout[start..]).into_owned())
    } else {
        None
    };
    LogInfo { bytes, tail }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real `nix derivation show … --json` output (Nix 2.34, enveloped form).
    const ENVELOPED: &str = r#"{"derivations":{"8bdr4r4sfa7s5aadl2k2bvmbyl1lyvm1-otel-ingest-test2.drv":{"args":["-c","echo building"],"builder":"/bin/sh","env":{},"inputs":{"drvs":{},"srcs":[]},"name":"otel-ingest-test2","outputs":{"out":{"path":"/nix/store/pah11i66zkkxgz5schca0p89qylmsgwy-otel-ingest-test2"}},"system":"x86_64-linux","version":4}},"version":4}"#;

    #[test]
    fn parse_enveloped_derivation_show() {
        let drv = "/nix/store/8bdr4r4sfa7s5aadl2k2bvmbyl1lyvm1-otel-ingest-test2.drv";
        let info = parse_derivation_show(ENVELOPED.as_bytes(), drv).expect("parsed");
        assert_eq!(info.name, "otel-ingest-test2");
        assert_eq!(info.system, "x86_64-linux");
        assert_eq!(
            info.outputs,
            "out=/nix/store/pah11i66zkkxgz5schca0p89qylmsgwy-otel-ingest-test2"
        );
    }

    #[test]
    fn parse_legacy_bare_map() {
        // Older nix emits the drv map directly (no "derivations" envelope) and
        // sometimes a bare hash-name path in outputs.
        let legacy = r#"{"/nix/store/zzz-foo.drv":{"name":"foo","system":"aarch64-linux","outputs":{"out":{"path":"abc-foo"}}}}"#;
        let info = parse_derivation_show(legacy.as_bytes(), "/nix/store/zzz-foo.drv").unwrap();
        assert_eq!(info.name, "foo");
        assert_eq!(info.system, "aarch64-linux");
        assert_eq!(info.outputs, "out=/nix/store/abc-foo");
    }

    #[test]
    fn parse_garbage_returns_none() {
        assert!(parse_derivation_show(b"not json", "/nix/store/x.drv").is_none());
    }

    #[test]
    fn log_tail_only_on_failure() {
        let body = b"line1\nline2\nfinal error here\n";
        let ok = log_info_from_bytes(body, false);
        assert_eq!(ok.bytes, body.len() as u64);
        assert!(ok.tail.is_none(), "no tail on success");

        let fail = log_info_from_bytes(body, true);
        assert_eq!(fail.bytes, body.len() as u64);
        assert_eq!(fail.tail.as_deref(), Some("line1\nline2\nfinal error here\n"));
    }

    #[test]
    fn log_tail_truncates_to_cap() {
        let big = vec![b'x'; LOG_TAIL_BYTES * 2];
        let info = log_info_from_bytes(&big, true);
        assert_eq!(info.bytes, (LOG_TAIL_BYTES * 2) as u64);
        assert_eq!(info.tail.as_ref().unwrap().len(), LOG_TAIL_BYTES);
    }
}
