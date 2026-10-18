//! Cachix daemon lifecycle and per-build push. Mirrors `run_cachix_daemon()`
//! at `__init__.py:813-857` and `Build.upload_cachix()` at `:949-967`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::{Child, Command};

use crate::build::Build;
use crate::options::{Options, maybe_remote, nix_shell};
use crate::stop::ensure_stop;

pub struct CachixDaemon {
    pub socket_path: PathBuf,
    child: Child,
    cmd_display: String,
    opts_for_stop: Options,
}

impl CachixDaemon {
    pub async fn start(tmp_dir: &Path, cachix_cache: &str, opts: &Options) -> Result<Self> {
        let socket_path = tmp_dir.join("cachix.sock");
        let mut cmd = nix_shell("nixpkgs#cachix", "cachix");
        cmd.extend([
            "daemon".to_string(),
            "run".to_string(),
            "--socket".to_string(),
            socket_path.to_string_lossy().into_owned(),
            cachix_cache.to_string(),
        ]);
        let cmd = maybe_remote(cmd, opts);
        let display = crate::stop::display_cmd(&cmd);
        let child = Command::new(&cmd[0])
            .args(&cmd[1..])
            .spawn()
            .with_context(|| format!("spawn {}", cmd[0]))?;

        // Wait for the socket to appear.
        for _ in 0..50 {
            if socket_path.exists() {
                return Ok(Self {
                    socket_path,
                    child,
                    cmd_display: display,
                    opts_for_stop: opts.clone(),
                });
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        bail!(
            "cachix daemon socket {} never appeared",
            socket_path.display()
        );
    }

    /// Tell the daemon to drain and exit, then ensure_stop the process.
    pub async fn stop(mut self) {
        let mut stop_cmd = nix_shell("nixpkgs#cachix", "cachix");
        stop_cmd.extend([
            "daemon".into(),
            "stop".into(),
            "--socket".into(),
            self.socket_path.to_string_lossy().into_owned(),
        ]);
        let stop_cmd = maybe_remote(stop_cmd, &self.opts_for_stop);
        let _ = Command::new(&stop_cmd[0])
            .args(&stop_cmd[1..])
            .status()
            .await;
        ensure_stop(&mut self.child, &self.cmd_display).await;
    }
}

pub async fn push(build: &Build, socket: &Path, opts: &Options) -> Result<i32> {
    if build.outputs.is_empty() {
        return Ok(0);
    }
    let mut cmd = nix_shell("nixpkgs#cachix", "cachix");
    cmd.extend([
        "daemon".to_string(),
        "push".to_string(),
        "--socket".to_string(),
        socket.to_string_lossy().into_owned(),
    ]);
    cmd.extend(build.outputs.values().cloned());
    let cmd = maybe_remote(cmd, opts);
    let status = Command::new(&cmd[0])
        .args(&cmd[1..])
        .status()
        .await
        .context("cachix daemon push")?;
    Ok(status.code().unwrap_or(-1))
}
