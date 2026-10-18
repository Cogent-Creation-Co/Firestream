//! Pattern #16 — `_build/<run>/manifest.json` schema. The **stable contract**
//! between the build system and downstream consumers. Mirrors the bash
//! `_phase3_export_target` + `finalize_manifest` (`bin/build/_common.sh`
//! lines 285-531).
//!
//! ## Schema (schema_version = 2)
//!
//! ```text
//! {
//!   "schema_version": 2,
//!   "run": {
//!     "date": "YYYY-MM-DD",
//!     "sha": "abc12345",
//!     "epoch": 1716831234,
//!     "branch": "main",
//!     "arch": "x86_64",
//!     "mode": "release",
//!     "host_os": "linux"
//!   },
//!   "artifacts": [ {Entry}, ... ]
//! }
//! ```
//!
//! Entries are streamed to `<rundir>/.manifest.entries.jsonl` (NDJSON) via
//! `record` so concurrent producers don't race on the canonical
//! `manifest.json`. `finalize` reads the entries, merges with any existing
//! `manifest.json` (newest-by-name wins, matching the bash), and atomically
//! writes the canonical file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::rundir::RunDir;

/// Schema version. Bumping is breaking — downstream consumers gate on this.
/// v2: `Outcome` enum serialises as `passed/failed/aborted` (was
/// `ok/failed/aborted` in v1) so the manifest field matches the verb used in
/// the live terminal and OTel spans.
pub const SCHEMA_VERSION: u32 = 2;

const ENTRIES_FILE: &str = ".manifest.entries.jsonl";
const MANIFEST_FILE: &str = "manifest.json";

#[derive(Debug, Error)]
pub enum Error {
    #[error("manifest: I/O error on `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("manifest: JSON error in `{path}`: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// Kind taxonomy for an artifact. Open enum: a string-typed `Other` keeps
/// callers from forking the type when they need a new kind one-off. The
/// bash variants are `image | binary | wasm | sbom | app` — we model them
/// as typed variants and add `Nix` + `Container` as more explicit synonyms
/// downstream consumers asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// Single OCI image tarball (`.tar.gz`).
    Image,
    /// Container image (alias for `Image`, used when the producer is a
    /// container build rather than a Nix derivation).
    Container,
    /// A built binary (or directory of binaries).
    Binary,
    /// WASM build output.
    Wasm,
    /// Software bill of materials (`*.json`).
    Sbom,
    /// macOS `.app` bundle or other application directory.
    App,
    /// A raw Nix store path artifact.
    Nix,
    /// Open variant for callers that need a kind not covered above.
    Other(String),
}

impl ArtifactKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Image => "image",
            Self::Container => "container",
            Self::Binary => "binary",
            Self::Wasm => "wasm",
            Self::Sbom => "sbom",
            Self::App => "app",
            Self::Nix => "nix",
            Self::Other(s) => s.as_str(),
        }
    }
}

/// One artifact entry. `attrs` is a free-form bag so callers can attach
/// metadata (store path, checksum, etc.) without forcing schema churn.
/// The bash `_append_manifest_entry` writes the same shape: attr, name,
/// kind, store_path, dest, sha256, size_bytes, success, error, ts_unix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub kind: ArtifactKind,
    pub dest_subdir: PathBuf,
    pub success: bool,
    #[serde(default)]
    pub attrs: HashMap<String, String>,
    /// Unix seconds. Populated by `record` when the caller omits it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts_unix: Option<u64>,
}

impl Entry {
    pub fn new(kind: ArtifactKind, dest_subdir: impl Into<PathBuf>, success: bool) -> Self {
        Self {
            kind,
            dest_subdir: dest_subdir.into(),
            success,
            attrs: HashMap::new(),
            ts_unix: None,
        }
    }

    pub fn with_attr(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.attrs.insert(k.into(), v.into());
        self
    }

    /// Logical name of the entry — `basename(dest_subdir)`. Used by
    /// `finalize` to dedup against an existing manifest (newest wins).
    pub fn name(&self) -> String {
        self.dest_subdir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    }
}

/// Terminal disposition of a run. Recorded in `RunMeta.outcome` so a
/// downstream consumer can tell a clean pass from a crashed/aborted run
/// (which still wrote a `manifest.json` via the scopeguard finalizer). The
/// field is `Option<>` for schema compatibility — older manifests parse
/// without it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Passed,
    Failed,
    Aborted,
}

/// Per-run metadata embedded in `manifest.json` under `run`. Populated by
/// `finalize` from the rundir layout + caller-provided context.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunMeta {
    pub date: String,
    pub sha: String,
    pub epoch: u64,
    pub branch: String,
    pub arch: String,
    pub mode: String,
    pub host_os: String,
    /// Terminal disposition. `None` for legacy manifests written before
    /// the field existed; new callers populate it via `finalize_manifest`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
}

/// Top-level file shape. Public so consumers can deserialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestFile {
    pub schema_version: u32,
    pub run: RunMeta,
    pub artifacts: Vec<Entry>,
}

/// Streaming manifest writer + finalizer. Cheap to construct; holds the
/// rundir path + buffered entries.
pub struct Manifest {
    rundir: PathBuf,
    entries: Vec<Entry>,
    /// Caller-provided run metadata. Set via `with_run_meta` before
    /// `finalize`; sensible defaults are filled in otherwise.
    run_meta: Option<RunMeta>,
}

impl Manifest {
    /// Open a manifest writer rooted at a `RunDir`. Creates the rundir
    /// path if missing (idempotent).
    pub async fn open(rundir: &RunDir) -> Result<Self, Error> {
        let path = rundir.path().to_path_buf();
        fs::create_dir_all(&path)
            .await
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
        Ok(Self {
            rundir: path,
            entries: Vec::new(),
            run_meta: None,
        })
    }

    /// Variant of `open` that takes a raw path. Useful when the rundir is
    /// inherited from env (container-side) and we don't have a `RunDir`
    /// handle.
    pub async fn open_at(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        fs::create_dir_all(&path)
            .await
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
        Ok(Self {
            rundir: path,
            entries: Vec::new(),
            run_meta: None,
        })
    }

    pub fn with_run_meta(mut self, meta: RunMeta) -> Self {
        self.run_meta = Some(meta);
        self
    }

    /// Append one entry to the streaming `.manifest.entries.jsonl` file
    /// AND to the in-memory buffer. Streaming keeps concurrent producers
    /// from racing on `manifest.json` (the bash uses `>>` for the same
    /// reason).
    ///
    /// Concurrent callers may race against the same file. We serialize the
    /// JSON payload **and** its trailing newline into a single buffer and
    /// issue one `write_all`, so the O_APPEND write is atomic at EOF.
    /// POSIX guarantees this for writes ≤ PIPE_BUF (4 KiB on Linux); entries
    /// run ~200 bytes, well under the limit. Doing the JSON and newline as
    /// separate writes used to interleave producers and corrupt the file
    /// (two JSON objects on one line, breaking `load_entries`).
    pub async fn record(&mut self, mut entry: Entry) -> Result<(), Error> {
        if entry.ts_unix.is_none() {
            entry.ts_unix = Some(now_unix());
        }
        let entries_path = self.rundir.join(ENTRIES_FILE);
        let mut payload = serde_json::to_vec(&entry).map_err(|source| Error::Json {
            path: entries_path.clone(),
            source,
        })?;
        payload.push(b'\n');
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&entries_path)
            .await
            .map_err(|source| Error::Io {
                path: entries_path.clone(),
                source,
            })?;
        f.write_all(&payload).await.map_err(|source| Error::Io {
            path: entries_path.clone(),
            source,
        })?;
        f.flush().await.map_err(|source| Error::Io {
            path: entries_path.clone(),
            source,
        })?;

        self.entries.push(entry);
        Ok(())
    }

    /// Read back all entries from the streaming NDJSON file. Useful for
    /// downstream consumers who skipped `record` and only want to inspect
    /// the file shape, and for `finalize`'s merge step.
    ///
    /// The manifest is descriptive (it records what was built); a single
    /// corrupt line should not mask the underlying CI exit status. So we
    /// log and skip bad lines instead of aborting. The writer fix in
    /// `record` is the real cure — this is defensive.
    pub async fn load_entries(&self) -> Result<Vec<Entry>, Error> {
        let path = self.rundir.join(ENTRIES_FILE);
        let bytes = match fs::read(&path).await {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => {
                return Err(Error::Io {
                    path: path.clone(),
                    source,
                });
            }
        };
        let mut out = Vec::new();
        for (lineno, line) in bytes.split(|b| *b == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            match serde_json::from_slice::<Entry>(line) {
                Ok(entry) => out.push(entry),
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        line = lineno + 1,
                        error = %e,
                        "manifest: skipping malformed entry line"
                    );
                }
            }
        }
        Ok(out)
    }

    /// Collate streaming entries + any existing `manifest.json` into the
    /// canonical file. Newest-by-name wins (matches the bash `group_by
    /// .name | map(.[-1])`). Returns the path written.
    pub async fn finalize(&self) -> Result<PathBuf, Error> {
        let manifest_path = self.rundir.join(MANIFEST_FILE);

        // Existing artifacts (other runs against the same rundir, e.g.
        // ci-linux.sh then ci-darwin.sh on the same host).
        let existing: Vec<Entry> = match fs::read(&manifest_path).await {
            Ok(bytes) => {
                let file: ManifestFile =
                    serde_json::from_slice(&bytes).map_err(|source| Error::Json {
                        path: manifest_path.clone(),
                        source,
                    })?;
                file.artifacts
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(source) => {
                return Err(Error::Io {
                    path: manifest_path.clone(),
                    source,
                });
            }
        };

        // Merge: existing + streaming + in-memory; last-wins by name.
        let streaming = self.load_entries().await?;
        let mut by_name: std::collections::BTreeMap<String, Entry> =
            std::collections::BTreeMap::new();
        for e in existing
            .into_iter()
            .chain(streaming)
            .chain(self.entries.iter().cloned())
        {
            by_name.insert(e.name(), e);
        }
        let artifacts: Vec<Entry> = by_name.into_values().collect();

        // Field-merge provided `run_meta` with `infer_run_meta(rundir)`.
        // The merge is "prefer non-empty caller-provided value, fall back
        // to inferred". This keeps a caller that sets only `branch` + `mode`
        // from inadvertently blanking `host_os`/`arch` (`RunMeta::default()`
        // has those as `""`) — a real regression that motivated this fix.
        let inferred = infer_run_meta(&self.rundir);
        let run = match self.run_meta.clone() {
            Some(provided) => merge_run_meta(provided, inferred),
            None => inferred,
        };

        let file = ManifestFile {
            schema_version: SCHEMA_VERSION,
            run,
            artifacts,
        };

        // Atomic write: tempfile + rename.
        let tmp = manifest_path.with_extension("json.tmp");
        let body = serde_json::to_vec_pretty(&file).map_err(|source| Error::Json {
            path: manifest_path.clone(),
            source,
        })?;
        fs::write(&tmp, &body).await.map_err(|source| Error::Io {
            path: tmp.clone(),
            source,
        })?;
        fs::rename(&tmp, &manifest_path)
            .await
            .map_err(|source| Error::Io {
                path: manifest_path.clone(),
                source,
            })?;
        Ok(manifest_path)
    }

    pub fn rundir(&self) -> &Path {
        &self.rundir
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Recover `date`/`epoch` from the rundir layout (`<base>/<date>/<sha>/<epoch>`)
/// when the caller didn't supply explicit metadata. The bash does the same
/// via a regex match against `BUILD_OUTPUT_DIR` (lines 488-495 of `_common.sh`).
fn infer_run_meta(rundir: &Path) -> RunMeta {
    let mut meta = RunMeta {
        host_os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        ..Default::default()
    };
    let mut comps: Vec<_> = rundir
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_string()),
            _ => None,
        })
        .collect();
    if comps.len() >= 3 {
        if let Some(epoch_str) = comps.pop() {
            meta.epoch = epoch_str.parse().unwrap_or(0);
        }
        if let Some(sha) = comps.pop() {
            meta.sha = sha;
        }
        if let Some(date) = comps.pop() {
            meta.date = date;
        }
    }
    meta
}

/// Field-merge two `RunMeta` values: prefer non-empty / non-zero `provided`
/// fields, fall back to `inferred`. `outcome` is taken verbatim from
/// `provided` (no inference exists for it; absent means absent).
fn merge_run_meta(provided: RunMeta, inferred: RunMeta) -> RunMeta {
    fn pick_str(p: String, i: String) -> String {
        if p.is_empty() { i } else { p }
    }
    RunMeta {
        date: pick_str(provided.date, inferred.date),
        sha: pick_str(provided.sha, inferred.sha),
        epoch: if provided.epoch != 0 {
            provided.epoch
        } else {
            inferred.epoch
        },
        branch: pick_str(provided.branch, inferred.branch),
        arch: pick_str(provided.arch, inferred.arch),
        mode: pick_str(provided.mode, inferred.mode),
        host_os: pick_str(provided.host_os, inferred.host_os),
        outcome: provided.outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn record_appends_to_jsonl_and_finalizes() {
        let base = tempdir().unwrap();
        let rundir = RunDir::create(base.path(), "abc12345").await.unwrap();

        let mut m = Manifest::open(&rundir).await.unwrap();
        m.record(Entry::new(ArtifactKind::Image, "demo-server", true))
            .await
            .unwrap();
        m.record(Entry::new(ArtifactKind::Wasm, "portal", true).with_attr("size", "1024"))
            .await
            .unwrap();

        // .manifest.entries.jsonl exists with 2 lines.
        let jsonl = rundir.path().join(ENTRIES_FILE);
        let body = std::fs::read_to_string(&jsonl).unwrap();
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("demo-server"));

        let path = m.finalize().await.unwrap();
        assert!(path.exists());
        let txt = std::fs::read_to_string(&path).unwrap();
        let file: ManifestFile = serde_json::from_str(&txt).unwrap();
        assert_eq!(file.schema_version, SCHEMA_VERSION);
        assert_eq!(file.artifacts.len(), 2);
    }

    #[tokio::test]
    async fn finalize_merges_existing_newest_wins() {
        let base = tempdir().unwrap();
        let rundir = RunDir::create(base.path(), "abc12345").await.unwrap();

        // Pre-write a manifest.json with one entry.
        let preexisting = ManifestFile {
            schema_version: SCHEMA_VERSION,
            run: RunMeta::default(),
            artifacts: vec![Entry::new(ArtifactKind::Image, "demo-server", false)],
        };
        std::fs::write(
            rundir.path().join(MANIFEST_FILE),
            serde_json::to_vec(&preexisting).unwrap(),
        )
        .unwrap();

        let mut m = Manifest::open(&rundir).await.unwrap();
        m.record(Entry::new(ArtifactKind::Image, "demo-server", true))
            .await
            .unwrap();
        m.finalize().await.unwrap();

        let txt = std::fs::read_to_string(rundir.path().join(MANIFEST_FILE)).unwrap();
        let file: ManifestFile = serde_json::from_str(&txt).unwrap();
        // newest (`true`) replaced the older (`false`) entry for the same name.
        assert_eq!(file.artifacts.len(), 1);
        assert!(file.artifacts[0].success);
    }

    #[tokio::test]
    async fn finalize_merges_partial_run_meta_with_inferred() {
        let base = tempdir().unwrap();
        let rundir = RunDir::create(base.path(), "deadbeef").await.unwrap();
        let m = Manifest::open(&rundir)
            .await
            .unwrap()
            .with_run_meta(RunMeta {
                branch: "main".into(),
                mode: "release".into(),
                ..Default::default()
            });
        m.finalize().await.unwrap();
        let file: ManifestFile =
            serde_json::from_slice(&std::fs::read(rundir.path().join("manifest.json")).unwrap())
                .unwrap();
        assert_eq!(file.run.branch, "main");
        assert_eq!(file.run.mode, "release");
        assert!(
            !file.run.host_os.is_empty(),
            "host_os must be inferred when not provided"
        );
        assert!(!file.run.arch.is_empty(), "arch must be inferred");
        assert_eq!(file.run.sha, "deadbeef");
        assert!(file.run.epoch > 0);
    }

    #[tokio::test]
    async fn finalize_infers_run_meta_from_rundir_layout() {
        let base = tempdir().unwrap();
        let rundir = RunDir::create(base.path(), "deadbeef").await.unwrap();
        let m = Manifest::open(&rundir).await.unwrap();
        m.finalize().await.unwrap();
        let txt = std::fs::read_to_string(rundir.path().join(MANIFEST_FILE)).unwrap();
        let file: ManifestFile = serde_json::from_str(&txt).unwrap();
        assert_eq!(file.run.sha, "deadbeef");
        assert!(file.run.epoch > 0, "epoch parsed from rundir layout");
        assert!(file.run.date.starts_with("20"));
    }

    #[tokio::test]
    async fn entry_kind_serializes_as_snake_case_with_other_string() {
        let e = Entry::new(ArtifactKind::Sbom, "x/y/sbom.json", true);
        let s = serde_json::to_string(&e).unwrap();
        assert!(s.contains(r#""kind":"sbom""#), "{s}");
        let e2 = Entry::new(ArtifactKind::Other("custom-format".into()), "x/y", true);
        let s = serde_json::to_string(&e2).unwrap();
        assert!(s.contains("custom-format"), "{s}");
    }

    #[tokio::test]
    async fn record_timestamp_is_populated_when_omitted() {
        let base = tempdir().unwrap();
        let rundir = RunDir::create(base.path(), "abc12345").await.unwrap();
        let mut m = Manifest::open(&rundir).await.unwrap();
        let before = now_unix();
        m.record(Entry::new(ArtifactKind::Binary, "bin/cli", true))
            .await
            .unwrap();
        let entries = m.load_entries().await.unwrap();
        assert_eq!(entries.len(), 1);
        let ts = entries[0].ts_unix.unwrap();
        assert!(ts >= before);
    }

    #[tokio::test]
    async fn open_at_works_without_rundir_handle() {
        let dir = tempdir().unwrap();
        let custom = dir.path().join("freeform");
        let mut m = Manifest::open_at(&custom).await.unwrap();
        m.record(Entry::new(ArtifactKind::Binary, "a", true))
            .await
            .unwrap();
        m.finalize().await.unwrap();
        assert!(custom.join(MANIFEST_FILE).exists());
    }

    /// Regression: producers used to do `write(json) + write("\n")` as two
    /// syscalls under `O_APPEND`. Each `write` is atomic at EOF on its own,
    /// but the pair is not, so concurrent writers interleaved and produced
    /// `{...A...}{...B...}\n\n` — `load_entries` then died with "trailing
    /// characters". Verify that N concurrent records produce N parseable
    /// JSONL lines.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn record_is_atomic_under_concurrent_writers() {
        let dir = tempdir().unwrap();
        let rundir = dir.path().join("rundir");
        std::fs::create_dir_all(&rundir).unwrap();

        const N: usize = 64;
        let mut handles = Vec::with_capacity(N);
        for i in 0..N {
            let rundir = rundir.clone();
            handles.push(tokio::spawn(async move {
                let mut m = Manifest::open_at(&rundir).await.unwrap();
                let mut e = Entry::new(ArtifactKind::Binary, format!("bin-{i}"), true);
                // Pad to a realistic size so the test exercises the same
                // size class as a real manifest entry (~200B).
                e = e
                    .with_attr("store_path", format!("/nix/store/aaaa-bin-{i}"))
                    .with_attr("sha256", "deadbeefcafebabe1234567890abcdef")
                    .with_attr("size_bytes", "123456")
                    .with_attr("note", "padding-padding-padding-padding");
                m.record(e).await.unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        // Every line must be valid JSON.
        let body = std::fs::read_to_string(rundir.join(ENTRIES_FILE)).unwrap();
        let mut parsed = 0;
        for line in body.lines() {
            assert!(!line.is_empty(), "empty line in JSONL");
            let _: Entry =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("bad line: {line:?}: {e}"));
            parsed += 1;
        }
        assert_eq!(parsed, N, "expected {N} entries, got {parsed}");

        // load_entries should agree.
        let m = Manifest::open_at(&rundir).await.unwrap();
        let entries = m.load_entries().await.unwrap();
        assert_eq!(entries.len(), N);
    }

    /// `load_entries` should skip a corrupt line instead of aborting — the
    /// manifest is descriptive, not load-bearing.
    #[tokio::test]
    async fn load_entries_skips_corrupt_lines() {
        let dir = tempdir().unwrap();
        let rundir = dir.path().join("rundir");
        std::fs::create_dir_all(&rundir).unwrap();

        let good = Entry::new(ArtifactKind::Binary, "bin/cli", true);
        let good_json = serde_json::to_string(&good).unwrap();
        let body = format!("{good_json}\nnot json at all\n{good_json}\n");
        std::fs::write(rundir.join(ENTRIES_FILE), body).unwrap();

        let m = Manifest::open_at(&rundir).await.unwrap();
        let entries = m.load_entries().await.unwrap();
        assert_eq!(entries.len(), 2, "two parseable entries survive");
    }
}
