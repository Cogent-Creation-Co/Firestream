//! Remote-builder support: temporary directories on the remote host and
//! source upload via `nix flake archive` / `nix copy`.
//!
//! Mirrors Python `remote_temp_dir()` (`__init__.py:708-727`),
//! `upload_sources()` (`__init__.py:625-679`), and the flake-metadata helpers
//! at `__init__.py:589-622`.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

use crate::options::Options;

pub struct RemoteTempDir {
    pub path: PathBuf,
    pub remote: String,
    pub ssh_options: Vec<String>,
}

impl RemoteTempDir {
    pub async fn create(opts: &Options) -> Result<Self> {
        let remote = opts
            .remote
            .as_ref()
            .context("remote not configured")?
            .clone();
        let mut cmd = vec!["ssh".to_string(), remote.clone()];
        cmd.extend(opts.remote_ssh_options.iter().cloned());
        cmd.push("--".into());
        cmd.push("mktemp".into());
        cmd.push("-d".into());

        let output = Command::new(&cmd[0])
            .args(&cmd[1..])
            .output()
            .await
            .context("ssh mktemp -d")?;
        if !output.status.success() {
            bail!(
                "Failed to create temporary directory on remote {remote}: rc={:?}",
                output.status.code()
            );
        }
        let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim().to_string());
        Ok(Self {
            path,
            remote,
            ssh_options: opts.remote_ssh_options.clone(),
        })
    }
}

impl Drop for RemoteTempDir {
    fn drop(&mut self) {
        // Fire-and-forget cleanup. We can't await in Drop; spawn a detached
        // task on the current runtime if one exists.
        let remote = self.remote.clone();
        let ssh_options = self.ssh_options.clone();
        let path = self.path.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let mut cmd = vec!["ssh".to_string(), remote];
                cmd.extend(ssh_options);
                cmd.push("--".into());
                cmd.push("rm".into());
                cmd.push("-rf".into());
                cmd.push(path.to_string_lossy().into_owned());
                let _ = Command::new(&cmd[0]).args(&cmd[1..]).output().await;
            });
        }
    }
}

/// Returns the resolved flake URL on the remote builder, uploading sources
/// when the flake has path-typed inputs that the remote couldn't substitute.
pub async fn upload_sources(opts: &Options) -> Result<String> {
    use serde_json::Value;

    // Fast path: ask nix for flake metadata; if neither the flake itself nor
    // any input is path-typed, we can build the flake URL directly.
    if !opts.always_upload_source {
        let meta = nix_flake_metadata(opts).await?;
        let url = meta
            .get("resolvedUrl")
            .and_then(|v| v.as_str())
            .unwrap_or(&opts.flake_url)
            .to_string();
        let has_path_inputs = check_for_path_inputs(&meta);
        let root_is_path = is_path_input(&meta);
        if !has_path_inputs && !root_is_path {
            return Ok(url);
        }
        if !has_path_inputs {
            // Only the root is path-typed; just copy the flake itself.
            let path = meta
                .get("path")
                .and_then(|v| v.as_str())
                .context("flake metadata missing path")?;
            let remote_url = opts.remote_url().context("remote not configured")?;
            let cmd = opts.nix_command(&["copy", "--to", &remote_url, "--no-check-sigs", path]);
            let cmd = crate::options::maybe_remote(cmd, opts);
            let env_ssh = opts.remote_ssh_options.join(" ");
            let status = Command::new(&cmd[0])
                .args(&cmd[1..])
                .env("NIX_SSHOPTS", env_ssh)
                .status()
                .await
                .context("nix copy")?;
            if !status.success() {
                bail!(
                    "failed to upload sources, nix copy exited with {:?}",
                    status.code()
                );
            }
            return Ok(path.to_string());
        }
    }

    // Slow path: full flake archive.
    let remote_url = opts.remote_url().context("remote not configured")?;
    let cmd = opts.nix_command(&[
        "flake",
        "archive",
        "--to",
        &remote_url,
        "--json",
        &opts.flake_url,
    ]);
    let cmd = crate::options::maybe_remote(cmd, opts);
    let output = Command::new(&cmd[0])
        .args(&cmd[1..])
        .output()
        .await
        .context("nix flake archive")?;
    if !output.status.success() {
        bail!("nix flake archive exited with {:?}", output.status.code());
    }
    let v: Value =
        serde_json::from_slice(&output.stdout).context("parse nix flake archive json")?;
    Ok(v.get("path")
        .and_then(|x| x.as_str())
        .context("nix flake archive json missing 'path'")?
        .to_string())
}

async fn nix_flake_metadata(opts: &Options) -> Result<serde_json::Value> {
    let cmd = opts.nix_command(&["flake", "metadata", "--json", &opts.flake_url]);
    let output = Command::new(&cmd[0])
        .args(&cmd[1..])
        .output()
        .await
        .context("nix flake metadata")?;
    if !output.status.success() {
        bail!("nix flake metadata exited with {:?}", output.status.code());
    }
    serde_json::from_slice(&output.stdout).context("parse nix flake metadata json")
}

fn is_path_input(node: &serde_json::Value) -> bool {
    let Some(locked) = node.get("locked") else {
        return false;
    };
    if locked.get("type").and_then(|v| v.as_str()) == Some("path") {
        return true;
    }
    locked
        .get("url")
        .and_then(|v| v.as_str())
        .map_or(false, |u| u.starts_with("file://"))
}

fn check_for_path_inputs(data: &serde_json::Value) -> bool {
    let Some(nodes) = data.pointer("/locks/nodes").and_then(|v| v.as_object()) else {
        return false;
    };
    nodes.values().any(is_path_input)
}
