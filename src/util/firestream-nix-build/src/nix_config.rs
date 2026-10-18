//! `nix config show --json` probe, used by CLI default resolution.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use tokio::process::Command;

/// Equivalent of Python `get_nix_config()` at `__init__.py:172-195`.
/// Returns a flat map of nix config keys to their string `value` field.
pub async fn get_nix_config(
    nix_bin: &[String],
    remote: Option<&str>,
    remote_ssh_options: &[String],
) -> Result<HashMap<String, String>> {
    let mut args: Vec<String> = nix_bin.iter().cloned().collect();
    args.push("--experimental-features".into());
    args.push("nix-command flakes".into());
    args.extend(["config", "show", "--json"].into_iter().map(String::from));

    let cmd = if let Some(remote_host) = remote {
        let joined = shlex::try_join(args.iter().map(String::as_str))
            .context("shell-quote nix config args")?;
        let mut out = vec!["ssh".to_string(), remote_host.to_string()];
        out.extend(remote_ssh_options.iter().cloned());
        out.push("--".into());
        out.push(joined);
        out
    } else {
        args
    };

    let output = Command::new(&cmd[0])
        .args(&cmd[1..])
        .output()
        .await
        .with_context(|| {
            let joined = shlex::try_join(cmd.iter().map(String::as_str)).unwrap_or_default();
            format!("nix not found in PATH, try to run {joined}")
        })?;

    if !output.status.success() {
        let joined = shlex::try_join(cmd.iter().map(String::as_str)).unwrap_or_default();
        bail!(
            "Failed to get nix config, {joined} exited with {:?}",
            output.status.code()
        );
    }

    // nix config show --json returns `{ "key": { "value": ..., ... } }`.
    let raw: serde_json::Value =
        serde_json::from_slice(&output.stdout).context("parse `nix config show --json`")?;
    let obj = raw
        .as_object()
        .context("nix config output is not a JSON object")?;

    let mut config = HashMap::new();
    for (key, entry) in obj {
        if let Some(v) = entry.get("value") {
            // The value field can be a string, bool, number, or array.
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Array(arr) => arr
                    .iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect::<Vec<_>>()
                    .join(" "),
                other => other.to_string(),
            };
            config.insert(key.clone(), s);
        }
    }
    Ok(config)
}
