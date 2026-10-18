//! Pattern #9 — `copy_source_to_container` via `git ls-files` (tracked +
//! untracked-but-not-ignored). Mirrors `bin/build/_common.sh::copy_source_to_container`
//! (lines 146-186) plus the `--others --exclude-standard` extension at
//! lines 167-174.
//!
//! The tracked + untracked union is what gives the container a Nix-source
//! hash that matches a standalone `nix build` of the same working tree —
//! that hash determinism is the whole point of the pattern.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::Command as TokioCommand;

use super::{DockerClient, Error};

/// The two `git ls-files` invocations the bash splices together. Exposed as
/// a type so callers can inspect the file set before sync (useful for
/// stale-source pruning in `devshell::stale_source_prune`).
pub struct GitLsFiles {
    pub tracked: Vec<PathBuf>,
    pub untracked: Vec<PathBuf>,
}

impl GitLsFiles {
    pub async fn collect(repo_root: &Path) -> Result<Self, Error> {
        let tracked = run_ls_files(repo_root, &["ls-files"]).await?;
        let untracked =
            run_ls_files(repo_root, &["ls-files", "--others", "--exclude-standard"]).await?;
        Ok(Self { tracked, untracked })
    }

    /// Both lists concatenated. The two are disjoint per `git`'s semantics
    /// (tracked vs untracked), so no dedup is needed; matching the bash
    /// comment exactly.
    pub fn union(&self) -> Vec<PathBuf> {
        let mut out = Vec::with_capacity(self.tracked.len() + self.untracked.len());
        out.extend(self.tracked.iter().cloned());
        out.extend(self.untracked.iter().cloned());
        out
    }
}

async fn run_ls_files(repo_root: &Path, args: &[&str]) -> Result<Vec<PathBuf>, Error> {
    let output = TokioCommand::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .await
        .map_err(|source| Error::Io {
            path: repo_root.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(Error::GitLsFiles {
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect())
}

/// Copy a `git ls-files` source set into a container as a tarball at
/// `dest_path` (e.g. `/tmp/src.tar`). The bash uses `tar -cf | docker cp`;
/// we stream through bollard's `upload_to_container` which accepts a tar
/// blob directly — same shape, one fewer file system round-trip.
///
/// `container_name` may be a name or an ID; bollard accepts either.
pub async fn copy_source_to_container(
    client: &DockerClient,
    container_name: &str,
    repo_root: &Path,
    dest_dir_in_container: &str,
    sources: &GitLsFiles,
) -> Result<usize, Error> {
    // Build the tar blob in-process. tar-rs handles GNU long-name support
    // automatically — important because submodule paths in this repo
    // exceed 100 bytes.
    let union = sources.union();
    let blob = build_tar_blob(repo_root, &union).await?;
    let bytes_written = blob.len();

    use bollard::container::UploadToContainerOptions;
    let options = UploadToContainerOptions {
        path: dest_dir_in_container.to_string(),
        no_overwrite_dir_non_dir: "false".to_string(),
    };
    client
        .inner()
        .upload_to_container(container_name, Some(options), blob.into())
        .await?;
    Ok(bytes_written)
}

/// Construct a tar blob in memory from the file list. Files that don't
/// exist on disk (e.g. tracked-but-deleted in the working tree) are
/// silently skipped — the bash uses `--ignore-failed-read` for the same
/// reason (line 160).
async fn build_tar_blob(repo_root: &Path, files: &[PathBuf]) -> Result<Vec<u8>, Error> {
    let repo_root = repo_root.to_path_buf();
    let files: Vec<PathBuf> = files.to_vec();
    tokio::task::spawn_blocking(move || -> Result<Vec<u8>, Error> {
        let mut buf = Vec::with_capacity(1 << 20);
        {
            let mut tar = tar::Builder::new(&mut buf);
            for rel in &files {
                let abs = repo_root.join(rel);
                let meta = match std::fs::metadata(&abs) {
                    Ok(m) => m,
                    Err(_) => continue, // mirror --ignore-failed-read
                };
                if meta.is_dir() {
                    let _ = tar.append_dir(rel, &abs);
                } else if meta.is_file() {
                    let mut f = std::fs::File::open(&abs).map_err(|source| Error::Io {
                        path: abs.clone(),
                        source,
                    })?;
                    let _ = tar.append_file(rel, &mut f);
                }
                // Symlinks: tar-rs would follow them via append_file; skip
                // to avoid duplicate-or-loop semantics. The bash tar
                // preserves them via its own logic; in practice the
                // ls-files-tracked set rarely contains them.
            }
            tar.finish().map_err(|source| Error::Io {
                path: PathBuf::from("<tar-builder>"),
                source,
            })?;
        }
        Ok(buf)
    })
    .await
    .map_err(|e| Error::Io {
        path: PathBuf::from("<spawn_blocking>"),
        source: std::io::Error::other(e),
    })?
}

/// CLI-shell version of `copy_source_to_container` for tests + the rare
/// case where the bollard upload path is unsuitable. Mirrors the bash's
/// `tar … | docker cp` pipeline.
#[allow(dead_code)]
pub(crate) async fn copy_source_via_cli(
    repo_root: &Path,
    container_name: &str,
    sources: &GitLsFiles,
) -> Result<(), Error> {
    let union = sources.union();
    let blob = build_tar_blob(repo_root, &union).await?;
    // docker cp - <container>:<dest>  with stdin = tar bytes.
    let mut child = TokioCommand::new("docker")
        .args(["cp", "-", &format!("{}:/tmp/src.tar", container_name)])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| Error::Io {
            path: PathBuf::from("docker"),
            source,
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(&blob).await.map_err(|source| Error::Io {
            path: PathBuf::from("docker stdin"),
            source,
        })?;
    }
    let output = child.wait_with_output().await.map_err(|source| Error::Io {
        path: PathBuf::from("docker"),
        source,
    })?;
    if !output.status.success() {
        return Err(Error::DockerCli {
            op: "cp",
            code: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    async fn init_repo(repo: &Path) {
        let _ = std::process::Command::new("git")
            .args(["init", "-q", repo.to_str().unwrap()])
            .output();
        let _ = std::process::Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "config", "user.email", "x@y"])
            .output();
        let _ = std::process::Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "config", "user.name", "x"])
            .output();
    }

    #[tokio::test]
    async fn collect_tracked_and_untracked_disjoint_union() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path();
        init_repo(repo).await;
        std::fs::write(repo.join("tracked.txt"), b"x").unwrap();
        let _ = std::process::Command::new("git")
            .args(["-C", repo.to_str().unwrap(), "add", "tracked.txt"])
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
        std::fs::write(repo.join("untracked.txt"), b"y").unwrap();
        std::fs::write(repo.join(".gitignore"), b"ignored.txt\n").unwrap();
        std::fs::write(repo.join("ignored.txt"), b"z").unwrap();

        let lsfs = GitLsFiles::collect(repo).await.unwrap();
        let union = lsfs.union();
        assert!(union.iter().any(|p| p == &PathBuf::from("tracked.txt")));
        assert!(union.iter().any(|p| p == &PathBuf::from("untracked.txt")));
        assert!(!union.iter().any(|p| p == &PathBuf::from("ignored.txt")));
    }

    #[tokio::test]
    async fn tar_blob_round_trip() {
        let tmp = tempdir().unwrap();
        let repo = tmp.path();
        std::fs::write(repo.join("hello.txt"), b"hello").unwrap();
        std::fs::create_dir_all(repo.join("subdir")).unwrap();
        std::fs::write(repo.join("subdir/inner.txt"), b"inner").unwrap();
        let files = vec![
            PathBuf::from("hello.txt"),
            PathBuf::from("subdir/inner.txt"),
        ];
        let blob = build_tar_blob(repo, &files).await.unwrap();
        assert!(!blob.is_empty());
        // Untar into a fresh dir and verify contents.
        let out = tempdir().unwrap();
        let mut ar = tar::Archive::new(blob.as_slice());
        ar.unpack(out.path()).unwrap();
        assert_eq!(
            std::fs::read(out.path().join("hello.txt")).unwrap(),
            b"hello"
        );
        assert_eq!(
            std::fs::read(out.path().join("subdir/inner.txt")).unwrap(),
            b"inner"
        );
    }

    #[tokio::test]
    async fn tar_blob_skips_missing_files_quietly() {
        let tmp = tempdir().unwrap();
        let files = vec![
            PathBuf::from("missing.txt"),
            PathBuf::from("also_missing.txt"),
        ];
        let blob = build_tar_blob(tmp.path(), &files).await.unwrap();
        // tar archive with only the trailing zero blocks — non-empty but
        // very small.
        assert!(blob.len() < 2048);
    }
}
