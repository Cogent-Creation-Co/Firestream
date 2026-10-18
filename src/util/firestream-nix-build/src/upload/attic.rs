//! Attic push. Mirrors `Build.upload_attic()` at `__init__.py:1008-1037` and
//! `_query_build_closure()` at `:969-1006`.

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::build::Build;
use crate::options::{Options, maybe_remote, nix_shell};

pub async fn push(build: &Build, opts: &Options) -> Result<i32> {
    let Some(cache) = &opts.attic_cache else {
        return Ok(0);
    };
    if build.outputs.is_empty() {
        return Ok(0);
    }
    let mut push_args = vec!["push".to_string()];
    if opts.attic_ignore_upstream_cache_filter {
        push_args.push("--ignore-upstream-cache-filter".into());
    }
    push_args.push(cache.clone());

    if opts.attic_push_build_closure {
        let paths = query_build_closure(&build.drv_path, &build.outputs, opts).await;
        push_args.push("--no-closure".into());
        push_args.extend(paths);
    } else {
        push_args.extend(build.outputs.values().cloned());
    }

    let mut cmd = nix_shell("nixpkgs#attic-client", "attic");
    cmd.extend(push_args);
    let cmd = maybe_remote(cmd, opts);
    let status = Command::new(&cmd[0])
        .args(&cmd[1..])
        .status()
        .await
        .context("attic push")?;
    Ok(status.code().unwrap_or(-1))
}

async fn query_build_closure(
    drv_path: &str,
    outputs: &std::collections::BTreeMap<String, String>,
    opts: &Options,
) -> Vec<String> {
    let cmd = vec![
        "nix-store".to_string(),
        "--query".into(),
        "--requisites".into(),
        "--include-outputs".into(),
        drv_path.to_string(),
    ];
    let cmd = maybe_remote(cmd, opts);
    match Command::new(&cmd[0]).args(&cmd[1..]).output().await {
        Ok(out) if out.status.success() => {
            let paths: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|p| !p.is_empty() && !p.ends_with(".drv"))
                .map(String::from)
                .collect();
            if paths.is_empty() {
                outputs.values().cloned().collect()
            } else {
                paths
            }
        }
        _ => outputs.values().cloned().collect(),
    }
}
