//! Pattern #20 — multi-format version-file manifest. Mechanism only: the
//! `(FileKind, PathBuf)` entries are project data.
//! Mirrors `bin/check-version.sh` (parity) and `bin/set-version.sh` (atomic
//! set). ConceptDB's `bin/_version-files.sh` was the equivalent path list.
//!
//! All-or-nothing semantics on `Manifest::set`: every file is staged into
//! a per-file tempfile in the same directory, every tempfile is fsync'd,
//! and only after all stages succeed do we `rename` each into place. A
//! single failure rolls everything back by restoring the originals
//! (captured in memory before the writes begin).

mod format;

pub use format::{CargoToml, FileFormat, PackageJson, Pyproject};

use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("version: invalid semver `{0}` (expected X.Y.Z or X.Y.Z-prerelease)")]
    InvalidSemver(String),

    #[error("version: file `{path}` missing")]
    Missing { path: PathBuf },

    #[error("version: parse error in `{path}`: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: anyhow::Error,
    },

    #[error("version: no version field found in `{path}`")]
    NoVersionField { path: PathBuf },

    #[error("version: I/O error on `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("version: atomic write rolled back ({reason}); restored {restored} of {total} files")]
    AtomicRollback {
        reason: String,
        restored: usize,
        total: usize,
    },
}

/// What format a path is. Selects the [`FileFormat`] impl at runtime from
/// the (kind, path) pair so the manifest can be a single typed list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Cargo,
    PackageJson,
    Pyproject,
}

impl FileKind {
    fn handler(self) -> Box<dyn FileFormat> {
        match self {
            Self::Cargo => Box::new(CargoToml),
            Self::PackageJson => Box::new(PackageJson),
            Self::Pyproject => Box::new(Pyproject),
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    kind: FileKind,
    path: PathBuf,
}

/// Caller-built list of `(FileKind, path)` entries. Path is interpreted
/// relative to the current working directory; callers should pre-join
/// against the repo root.
#[derive(Debug, Default, Clone)]
pub struct Manifest {
    entries: Vec<Entry>,
}

impl Manifest {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(mut self, kind: FileKind, path: impl AsRef<Path>) -> Self {
        self.entries.push(Entry {
            kind,
            path: path.as_ref().to_path_buf(),
        });
        self
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Verify every entry's current version matches `expected`. Files that
    /// don't exist are recorded as parity mismatches with the placeholder
    /// "<missing>" (mirrors `check-version.sh`, which reports MISSING but
    /// does not abort).
    pub fn check(&self, expected: &str) -> Result<ParityReport, Error> {
        validate_semver(expected)?;
        let mut report = ParityReport::default();
        for e in &self.entries {
            if !e.path.exists() {
                report
                    .mismatches
                    .push((e.path.clone(), "<missing>".to_string()));
                continue;
            }
            let handler = e.kind.handler();
            match handler.read(&e.path) {
                Ok(actual) if actual == expected => {
                    report.matches.push(e.path.clone());
                }
                Ok(actual) => {
                    report.mismatches.push((e.path.clone(), actual));
                }
                Err(err) => return Err(err),
            }
        }
        Ok(report)
    }

    /// Atomically update every entry to `new`. Three-phase commit:
    ///   1. Validate `new` as semver; read originals; render new bodies.
    ///   2. Write all new bodies to per-file tempfiles + fsync.
    ///   3. Rename each tempfile into place. On a mid-phase-3 failure,
    ///      previously-renamed files are restored from in-memory originals.
    pub fn set(&self, new: &str) -> Result<(), Error> {
        validate_semver(new)?;

        // Phase 1: read originals + render new bodies. Originals are kept
        // for rollback; new bodies are kept so Phase 3 doesn't have to
        // re-run any format-specific logic.
        let mut staged: Vec<Staged> = Vec::with_capacity(self.entries.len());
        for e in &self.entries {
            if !e.path.exists() {
                return Err(Error::Missing {
                    path: e.path.clone(),
                });
            }
            let original = std::fs::read(&e.path).map_err(|source| Error::Io {
                path: e.path.clone(),
                source,
            })?;
            let handler = e.kind.handler();
            let new_body = handler.render(&original, new, &e.path)?;
            staged.push(Staged {
                entry: e.clone(),
                original,
                new_body,
                tmp_path: tmp_sibling(&e.path),
            });
        }

        // Phase 2: stage every new body to a sibling tempfile. Failure here
        // aborts before touching any original — rollback is just unlinking
        // the tempfiles we already wrote.
        for (i, s) in staged.iter().enumerate() {
            if let Err(source) = write_and_sync(&s.tmp_path, &s.new_body) {
                cleanup_tempfiles(&staged[..=i]);
                return Err(Error::Io {
                    path: s.tmp_path.clone(),
                    source,
                });
            }
        }

        // Phase 3: rename each tempfile into place. This is the only step
        // that mutates originals; mid-loop failure restores from the
        // in-memory originals captured in Phase 1.
        for (i, s) in staged.iter().enumerate() {
            if let Err(source) = std::fs::rename(&s.tmp_path, &s.entry.path) {
                let restored = restore_originals(&staged[..i]);
                cleanup_tempfiles(&staged[i..]);
                return Err(Error::AtomicRollback {
                    reason: format!("rename {:?}: {}", s.tmp_path, source),
                    restored,
                    total: staged.len(),
                });
            }
        }

        Ok(())
    }
}

struct Staged {
    entry: Entry,
    original: Vec<u8>,
    new_body: Vec<u8>,
    tmp_path: PathBuf,
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tmp".to_string());
    // Per-pid suffix is enough: the only collision hazard is two concurrent
    // `set()` calls on the same file, which the caller is responsible for
    // ordering. Random suffix would only obscure the source.
    let pid = std::process::id();
    parent.join(format!(".{name}.firestream-ci.{pid}.tmp"))
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    f.write_all(bytes)?;
    f.flush()?;
    // Sync data so the Phase-3 rename can't expose a torn write under
    // crash. We don't fsync the parent dir — the rename does that implicitly
    // on every Linux FS that matters (the dirent update is journaled).
    f.sync_all()?;
    Ok(())
}

fn cleanup_tempfiles(staged: &[Staged]) {
    for s in staged {
        let _ = std::fs::remove_file(&s.tmp_path);
    }
}

fn restore_originals(staged: &[Staged]) -> usize {
    let mut restored = 0;
    for s in staged {
        // Best-effort: a failed restore here just leaves a half-updated
        // tree. The error returned to the caller already names this case.
        if std::fs::write(&s.entry.path, &s.original).is_ok() {
            restored += 1;
        }
    }
    restored
}

#[derive(Debug, Default, Clone)]
pub struct ParityReport {
    pub matches: Vec<PathBuf>,
    pub mismatches: Vec<(PathBuf, String)>,
}

impl ParityReport {
    pub fn ok(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// Same regex shape as `bin/set-version.sh`:
///   ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$
pub fn validate_semver(s: &str) -> Result<(), Error> {
    let (core, pre) = match s.split_once('-') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 {
        return Err(Error::InvalidSemver(s.into()));
    }
    for p in &parts {
        if p.is_empty() || !p.chars().all(|c| c.is_ascii_digit()) {
            return Err(Error::InvalidSemver(s.into()));
        }
    }
    if let Some(pre) = pre {
        if pre.is_empty() || !pre.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') {
            return Err(Error::InvalidSemver(s.into()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn semver_validation() {
        assert!(validate_semver("1.2.3").is_ok());
        assert!(validate_semver("0.0.1").is_ok());
        assert!(validate_semver("10.20.30").is_ok());
        assert!(validate_semver("1.2.3-rc.1").is_ok());
        assert!(validate_semver("1.2.3-alpha").is_ok());
        assert!(validate_semver("1.2").is_err());
        assert!(validate_semver("1.2.3.4").is_err());
        assert!(validate_semver("v1.2.3").is_err());
        assert!(validate_semver("1.2.-3").is_err());
    }

    #[test]
    fn check_reports_mismatch_and_matches() {
        let dir = tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        std::fs::write(
            &cargo,
            "[workspace.package]\nversion = \"1.2.3\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let pkg = dir.path().join("package.json");
        std::fs::write(&pkg, "{\"name\":\"x\",\"version\":\"9.9.9\"}\n").unwrap();

        let m = Manifest::new()
            .add(FileKind::Cargo, &cargo)
            .add(FileKind::PackageJson, &pkg);
        let report = m.check("1.2.3").unwrap();
        assert_eq!(report.matches, vec![cargo.clone()]);
        assert_eq!(report.mismatches, vec![(pkg.clone(), "9.9.9".into())]);
        assert!(!report.ok());
    }

    #[test]
    fn set_atomic_succeeds_across_all_kinds() {
        let dir = tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        std::fs::write(
            &cargo,
            "[workspace.package]\nversion = \"0.0.1\"\nedition = \"2021\"\n",
        )
        .unwrap();
        let pkg = dir.path().join("package.json");
        std::fs::write(&pkg, "{\n  \"name\": \"x\",\n  \"version\": \"0.0.1\"\n}\n").unwrap();
        let py = dir.path().join("pyproject.toml");
        std::fs::write(&py, "[project]\nname = \"x\"\nversion = \"0.0.1\"\n").unwrap();

        let m = Manifest::new()
            .add(FileKind::Cargo, &cargo)
            .add(FileKind::PackageJson, &pkg)
            .add(FileKind::Pyproject, &py);
        m.set("1.2.3").unwrap();

        let report = m.check("1.2.3").unwrap();
        assert!(report.ok(), "mismatches: {:?}", report.mismatches);
    }

    #[test]
    fn set_rejects_bad_semver_without_writing() {
        let dir = tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        std::fs::write(&cargo, "[workspace.package]\nversion = \"1.0.0\"\n").unwrap();
        let original = std::fs::read(&cargo).unwrap();
        let m = Manifest::new().add(FileKind::Cargo, &cargo);
        let err = m.set("not-a-semver").unwrap_err();
        assert!(matches!(err, Error::InvalidSemver(_)));
        assert_eq!(std::fs::read(&cargo).unwrap(), original);
    }

    #[test]
    fn set_rejects_missing_entry_without_writing() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("Cargo.toml");
        std::fs::write(&real, "[workspace.package]\nversion = \"1.0.0\"\n").unwrap();
        let missing = dir.path().join("does-not-exist.toml");
        let original = std::fs::read(&real).unwrap();

        let m = Manifest::new()
            .add(FileKind::Cargo, &real)
            .add(FileKind::Cargo, &missing);
        let err = m.set("2.0.0").unwrap_err();
        assert!(matches!(err, Error::Missing { .. }));
        // The first file must not have been touched.
        assert_eq!(std::fs::read(&real).unwrap(), original);
    }
}
