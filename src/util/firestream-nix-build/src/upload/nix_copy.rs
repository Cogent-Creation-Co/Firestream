//! `nix copy --to <uri>` for `--copy-to`. Mirrors `Build.upload` at
//! `__init__.py:930-947`.

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::build::Build;
use crate::options::{Options, maybe_remote};

pub async fn upload(build: &Build, opts: &Options) -> Result<i32> {
    let Some(copy_to) = &opts.copy_to else {
        return Ok(0);
    };
    if build.outputs.is_empty() {
        return Ok(0);
    }
    let mut args = opts.nix_command(&["copy", "--log-format", "raw", "--to", copy_to]);
    args.extend(build.outputs.values().cloned());
    let cmd = maybe_remote(args, opts);
    let status = Command::new(&cmd[0])
        .args(&cmd[1..])
        .status()
        .await
        .context("nix copy")?;
    Ok(status.code().unwrap_or(-1))
}
