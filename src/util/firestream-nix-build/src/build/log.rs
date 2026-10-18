//! `nix log <drv>` fallback for failed builds. Mirrors `Build.get_build_log`
//! at `__init__.py:899-917`.

use anyhow::Result;
use tokio::process::Command;

use crate::options::{Options, maybe_remote};

pub async fn get_build_log(drv_path: &str, opts: &Options) -> Result<String> {
    let cmd = opts.nix_command(&["log", drv_path]);
    let cmd = maybe_remote(cmd, opts);
    tracing::debug!(
        "run {}",
        shlex::try_join(cmd.iter().map(String::as_str)).unwrap_or_default()
    );

    match Command::new(&cmd[0]).args(&cmd[1..]).output().await {
        Ok(out) => {
            if out.status.success() && !out.stdout.is_empty() {
                Ok(String::from_utf8_lossy(&out.stdout).into_owned())
            } else if !out.stderr.is_empty() {
                Ok(String::from_utf8_lossy(&out.stderr).into_owned())
            } else {
                Ok(String::new())
            }
        }
        Err(e) => {
            tracing::debug!("Failed to get build log: {e}");
            Ok(String::new())
        }
    }
}
