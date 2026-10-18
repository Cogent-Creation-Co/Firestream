//! Pattern #5 — warm-bootstrap a Nix dev shell. Mirrors
//! `bin/_lib.sh::devshell_init_snippet` (lines 580-672).
//!
//! Flow:
//!   1. If `cache_path` (e.g., `/nix-keep/dev-env.sh`) exists, parse it
//!      and apply `export FOO=bar` / `FOO=bar` lines into the returned
//!      `DevShell.env` map.
//!   2. Probe each `required_tools` entry via PATH lookup.
//!   3. If any tool is missing OR the cache is absent, regenerate via
//!      `nix print-dev-env <flake-ref>` and write the cache.
//!   4. Re-parse the (possibly regenerated) cache.
//!
//! Additional bash-side dance handled here:
//!   * `stale_source_prune`: given a tarball file list (incoming) AND a
//!     `git ls-files` view of the current working tree, remove files that
//!     are tracked-but-not-incoming. The bash uses `comm -23` against
//!     sorted file lists.
//!   * `reinit_compact_git`: when `git rev-list --count HEAD > 10`, blow
//!     away `.git` and re-init to keep object count bounded across many
//!     CI runs. Mirrors lines 632-637.
//!   * `worktree_strip_git`: convert a worktree-style `.git` file pointer
//!     to a real `.git` dir by re-init (lines 611-616).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use thiserror::Error;
use tokio::fs;
use tokio::process::Command as TokioCommand;
use which::which as which_crate;

#[derive(Debug, Error)]
pub enum Error {
    #[error("devshell: I/O error on `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("devshell: nix print-dev-env failed ({code}): {stderr}")]
    NixPrintDevEnv { code: i32, stderr: String },
    #[error("devshell: failed to spawn `nix`: {0}")]
    SpawnNix(#[source] std::io::Error),
    #[error("devshell: git command failed ({code}): {stderr}")]
    Git { code: i32, stderr: String },
}

#[derive(Debug, Default, Clone)]
pub struct DevShell {
    /// Env vars produced by parsing the cache. The caller applies them
    /// (e.g., to a `Command::envs` builder) — we never mutate the
    /// process env directly because that breaks tests + concurrent use.
    pub env: HashMap<String, String>,
    /// True iff we ran `nix print-dev-env` during bootstrap. Lets the
    /// caller report "cold-start cost" in spans.
    pub regenerated: bool,
}

impl DevShell {
    /// Bootstrap a dev shell into memory:
    ///   * Probe `cache_path` (e.g. `/nix-keep/dev-env.sh`)
    ///   * Probe `required_tools` via `which`
    ///   * Regenerate cache if any tool is missing
    ///
    /// `flake_ref` is e.g. `".#devShells.${system}.default"`. The bash
    /// uses `nix eval --impure --expr builtins.currentSystem --raw` to
    /// fill in the system; we let the caller compose the full ref.
    pub async fn bootstrap(
        cache_path: &Path,
        required_tools: &[&str],
        flake_ref: &str,
    ) -> Result<Self, Error> {
        // 1. Parse cache if present.
        let mut env = if cache_path.exists() {
            parse_export_file(cache_path).await?
        } else {
            HashMap::new()
        };

        // 2. Probe tools using the parsed env's PATH if set; otherwise
        //    the process's PATH.
        let path_override = env.get("PATH").cloned();
        let missing = missing_tools(required_tools, path_override.as_deref());

        let regenerated = if !cache_path.exists() || !missing.is_empty() {
            // 3. Regenerate.
            regenerate_cache(cache_path, flake_ref).await?;
            env = parse_export_file(cache_path).await?;
            true
        } else {
            false
        };

        Ok(Self { env, regenerated })
    }

    /// Prune source files that were tracked previously but are absent
    /// from the incoming source set. Mirrors lines 622-625 of the bash:
    ///
    /// ```text
    /// tar -tf /tmp/src.tar | sort > /tmp/ci-incoming.txt
    /// git -C "$REPO" ls-files | sort > /tmp/ci-old.txt
    /// comm -23 /tmp/ci-old.txt /tmp/ci-incoming.txt | xargs rm -f
    /// ```
    ///
    /// Returns the list of paths removed (relative to `repo_root`).
    pub async fn stale_source_prune(
        repo_root: &Path,
        incoming: &[PathBuf],
    ) -> Result<Vec<PathBuf>, Error> {
        let incoming_set: BTreeSet<&Path> = incoming.iter().map(|p| p.as_path()).collect();

        // git ls-files (run synchronously via tokio Command)
        let output = TokioCommand::new("git")
            .arg("-C")
            .arg(repo_root)
            .arg("ls-files")
            .output()
            .await
            .map_err(|source| Error::Io {
                path: repo_root.to_path_buf(),
                source,
            })?;
        if !output.status.success() {
            return Err(Error::Git {
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let listed: Vec<PathBuf> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();

        // comm -23: in `listed` but not in `incoming_set`.
        let to_remove: Vec<PathBuf> = listed
            .into_iter()
            .filter(|p| !incoming_set.contains(p.as_path()))
            .collect();

        for rel in &to_remove {
            let abs = repo_root.join(rel);
            // Mirror `rm -f` semantics — ignore not-found.
            let _ = fs::remove_file(&abs).await;
        }
        Ok(to_remove)
    }

    /// When `git rev-list --count HEAD` exceeds `threshold` (bash uses 10),
    /// blow away `.git` and re-init with the bash's `ci-build` branch
    /// name + a fake committer identity. Idempotent — fails silently if
    /// the repo has no commits yet.
    pub async fn reinit_compact_git(repo_root: &Path, threshold: u64) -> Result<bool, Error> {
        let output = TokioCommand::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["rev-list", "--count", "HEAD"])
            .output()
            .await
            .map_err(|source| Error::Io {
                path: repo_root.to_path_buf(),
                source,
            })?;
        let count: u64 = if output.status.success() {
            String::from_utf8_lossy(&output.stdout)
                .trim()
                .parse()
                .unwrap_or(0)
        } else {
            0
        };
        if count <= threshold {
            return Ok(false);
        }
        // Remove .git and re-init.
        let dot_git = repo_root.join(".git");
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&dot_git)).await;
        init_ci_git_dir(repo_root).await?;
        Ok(true)
    }

    /// Convert a worktree `.git` *file* pointer into a real `.git`
    /// directory by removing it and re-initializing. Mirrors bash lines
    /// 611-616. Idempotent — does nothing if `.git` is already a dir.
    pub async fn worktree_strip_git(repo_root: &Path) -> Result<bool, Error> {
        let dot_git = repo_root.join(".git");
        if dot_git.is_dir() {
            return Ok(false);
        }
        if !dot_git.exists() {
            init_ci_git_dir(repo_root).await?;
            return Ok(true);
        }
        let _ = fs::remove_file(&dot_git).await;
        init_ci_git_dir(repo_root).await?;
        Ok(true)
    }
}

async fn init_ci_git_dir(repo_root: &Path) -> Result<(), Error> {
    let output = TokioCommand::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["init", "-b", "ci-build"])
        .output()
        .await
        .map_err(|source| Error::Io {
            path: repo_root.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(Error::Git {
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    // Config user.email / user.name so subsequent commits succeed without
    // a configured global identity.
    for (key, val) in [("user.email", "ci@firestream.local"), ("user.name", "CI")] {
        let _ = TokioCommand::new("git")
            .arg("-C")
            .arg(repo_root)
            .args(["config", key, val])
            .output()
            .await;
    }
    Ok(())
}

async fn regenerate_cache(cache_path: &Path, flake_ref: &str) -> Result<(), Error> {
    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
    }
    let output = TokioCommand::new("nix")
        .args(["print-dev-env", flake_ref, "--no-update-lock-file"])
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(Error::SpawnNix)?;
    if !output.status.success() {
        return Err(Error::NixPrintDevEnv {
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    fs::write(cache_path, &output.stdout)
        .await
        .map_err(|source| Error::Io {
            path: cache_path.to_path_buf(),
            source,
        })?;
    Ok(())
}

/// Parse a shell `export FOO=bar` / `FOO=bar` file into a key→value map.
/// Quoted values are stripped of their wrapping single or double quotes.
/// Lines that don't look like assignments are ignored.
async fn parse_export_file(path: &Path) -> Result<HashMap<String, String>, Error> {
    let body = fs::read_to_string(path).await.map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(parse_export_string(&body))
}

pub(crate) fn parse_export_string(body: &str) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    for raw_line in body.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let assignment = if let Some(rest) = line.strip_prefix("export ") {
            rest
        } else {
            line
        };
        let (k, v) = match assignment.split_once('=') {
            Some(p) => p,
            None => continue,
        };
        // Reject obviously-non-name keys; this also filters out things
        // like function declarations (`foo () {`).
        if !is_valid_var_name(k) {
            continue;
        }
        out.insert(k.to_string(), unquote_value(v));
    }
    out
}

fn is_valid_var_name(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn unquote_value(v: &str) -> String {
    let trimmed = v.trim();
    if trimmed.len() >= 2 {
        let first = trimmed.as_bytes()[0];
        let last = trimmed.as_bytes()[trimmed.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return trimmed[1..trimmed.len() - 1].to_string();
        }
    }
    trimmed.to_string()
}

fn missing_tools(required: &[&str], path_override: Option<&str>) -> Vec<String> {
    let mut missing = Vec::new();
    for tool in required {
        let found = match path_override {
            Some(p) => which_in_path(tool, p),
            None => which_crate(tool).is_ok(),
        };
        if !found {
            missing.push((*tool).to_string());
        }
    }
    missing
}

fn which_in_path(tool: &str, path_var: &str) -> bool {
    for dir in path_var.split(':') {
        let candidate: PathBuf = Path::new(dir).join(tool);
        if candidate.is_file() {
            return true;
        }
    }
    false
}

/// Sort + dedup a list — exposed for unit tests of the bash-equivalent
/// set-difference logic.
pub(crate) fn _sorted_dedup<T: Ord + Clone>(items: &[T]) -> Vec<T> {
    let set: BTreeMap<&T, ()> = items.iter().map(|i| (i, ())).collect();
    set.into_keys().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_export_lines() {
        let body = r#"
            export FOO=bar
            BAZ="qux quux"
            export EMPTY=
            export QUOTED_SINGLE='hello'
            # a comment
            badname-1=ignored
            FOO_BAR=ok_value
        "#;
        let m = parse_export_string(body);
        assert_eq!(m.get("FOO").unwrap(), "bar");
        assert_eq!(m.get("BAZ").unwrap(), "qux quux");
        assert_eq!(m.get("EMPTY").unwrap(), "");
        assert_eq!(m.get("QUOTED_SINGLE").unwrap(), "hello");
        assert_eq!(m.get("FOO_BAR").unwrap(), "ok_value");
        assert!(!m.contains_key("badname-1"));
    }

    #[tokio::test]
    async fn bootstrap_reads_cache_without_regen_if_tools_present() {
        let tmp = tempdir().unwrap();
        let cache = tmp.path().join("dev-env.sh");
        // Put a known-good binary in a tmp bin/ dir, write the cache to
        // reference it via PATH, and probe for that exact binary. This is
        // hermetic — the test passes regardless of the host's bin layout.
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir(&bin_dir).unwrap();
        let tool_path = bin_dir.join("synthetic-tool");
        std::fs::write(&tool_path, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let cache_body = format!("export FROM_CACHE=yes\nexport PATH={}\n", bin_dir.display());
        std::fs::write(&cache, cache_body).unwrap();
        let shell = DevShell::bootstrap(&cache, &["synthetic-tool"], ".#devShells.default")
            .await
            .unwrap();
        assert!(!shell.regenerated, "should not have regenerated");
        assert_eq!(shell.env.get("FROM_CACHE").unwrap(), "yes");
    }

    #[tokio::test]
    async fn stale_source_prune_removes_tracked_but_not_incoming() {
        // Build a synthetic git tree under tmp.
        let tmp = tempdir().unwrap();
        let repo = tmp.path();
        let _ = std::process::Command::new("git")
            .args(["init", "-q", repo.to_str().unwrap()])
            .output();
        let _ = std::process::Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "config", "user.email", "x@y"])
            .output();
        let _ = std::process::Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "config", "user.name", "x"])
            .output();
        std::fs::write(repo.join("kept.txt"), b"a").unwrap();
        std::fs::write(repo.join("removed.txt"), b"b").unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "add", "."])
            .output();
        let _ = std::process::Command::new("git")
            .args([
                "-C",
                repo.to_str().unwrap(),
                "commit",
                "-q",
                "--no-gpg-sign",
                "-m",
                "init",
            ])
            .output();

        // Incoming = only kept.txt.
        let incoming = vec![PathBuf::from("kept.txt")];
        let removed = DevShell::stale_source_prune(repo, &incoming).await.unwrap();
        assert_eq!(removed, vec![PathBuf::from("removed.txt")]);
        assert!(!repo.join("removed.txt").exists());
        assert!(repo.join("kept.txt").exists());
    }

    #[tokio::test]
    async fn reinit_compact_git_no_op_below_threshold() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path();
        let _ = std::process::Command::new("git")
            .args(["init", "-q", repo.to_str().unwrap()])
            .output();
        let _ = std::process::Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "config", "user.email", "x@y"])
            .output();
        let _ = std::process::Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "config", "user.name", "x"])
            .output();
        std::fs::write(repo.join("a"), b"x").unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "add", "."])
            .output();
        let _ = std::process::Command::new("git")
            .args([
                "-C",
                repo.to_str().unwrap(),
                "commit",
                "-q",
                "--no-gpg-sign",
                "-m",
                "init",
            ])
            .output();
        let did = DevShell::reinit_compact_git(repo, 10).await.unwrap();
        assert!(!did, "should not reinit below threshold");
    }

    #[tokio::test]
    async fn worktree_strip_git_replaces_file_with_dir() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path();
        // Write a fake worktree `.git` file pointer
        std::fs::write(repo.join(".git"), b"gitdir: /elsewhere").unwrap();
        let changed = DevShell::worktree_strip_git(repo).await.unwrap();
        assert!(changed);
        assert!(repo.join(".git").is_dir(), ".git is now a directory");
    }

    #[test]
    fn unquote_value_strips_matched_quotes_only() {
        assert_eq!(unquote_value("\"hello\""), "hello");
        assert_eq!(unquote_value("'world'"), "world");
        assert_eq!(unquote_value("mixed\"'end"), "mixed\"'end");
        assert_eq!(unquote_value("noquotes"), "noquotes");
    }

    #[test]
    fn missing_tools_detects_absent_with_path_override() {
        let missing = missing_tools(&["__definitely_not_a_real_tool__"], Some("/usr/bin"));
        assert_eq!(missing, vec!["__definitely_not_a_real_tool__".to_string()]);
    }
}
