//! `nix copy --from <remote>` to pull build results back from a remote
//! builder. Mirrors `Build.download` at `__init__.py:1039-1058`.

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::build::Build;
use crate::options::Options;

pub async fn download(build: &Build, opts: &Options) -> Result<i32> {
    let Some(remote_url) = opts.remote_url() else {
        return Ok(0);
    };
    if !opts.download || build.outputs.is_empty() {
        return Ok(0);
    }
    let mut args = opts.nix_command(&[
        "copy",
        "--log-format",
        "raw",
        "--no-check-sigs",
        "--from",
        &remote_url,
    ]);
    args.extend(build.outputs.values().cloned());
    let ssh_opts = opts.remote_ssh_options.join(" ");
    let status = Command::new(&args[0])
        .args(&args[1..])
        .env("NIX_SSHOPTS", ssh_opts)
        .status()
        .await
        .context("nix copy --from")?;
    Ok(status.code().unwrap_or(-1))
}
