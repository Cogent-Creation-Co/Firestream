//! niks3 batched push. Mirrors `run_niks3_upload()` at `__init__.py:1362-1421`.
//! Unlike the other uploaders, niks3 is a single batching worker — it pulls
//! one build, drains everything else currently queued, and pushes them in one
//! `niks3 push` call.

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::build::Build;
use crate::options::{Options, maybe_remote, nix_shell};

pub async fn push(builds: &[&Build], opts: &Options) -> Result<i32> {
    let server = opts
        .niks3_server
        .as_deref()
        .context("niks3-server not set")?;
    let mut all_outputs: Vec<String> = Vec::new();
    for b in builds {
        all_outputs.extend(b.outputs.values().cloned());
    }
    let mut cmd = nix_shell("github:Mic92/niks3", "niks3");
    cmd.push("push".into());
    cmd.extend(all_outputs);
    let cmd = maybe_remote(cmd, opts);
    let status = Command::new(&cmd[0])
        .args(&cmd[1..])
        .env("NIKS3_SERVER_URL", server)
        .status()
        .await
        .context("niks3 push")?;
    Ok(status.code().unwrap_or(-1))
}
