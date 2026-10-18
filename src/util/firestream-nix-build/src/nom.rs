//! `nix-output-monitor` (nom) integration. Equivalent of the custom Python
//! `Pipe` class + `nix_output_monitor()` at `__init__.py:35-51, 793-810`.
//!
//! Architecture: we open an OS pipe; the read end goes to `nom --json` as its
//! stdin (inherited fd), and the write end is handed to each `nix build`
//! worker so its stderr lines (which are `internal-json` records when
//! `--log-format internal-json -v` is in effect) can be forwarded to nom.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::process::{Child, Command};

use crate::options::{Options, maybe_remote, nix_shell};

/// A pair of pipe endpoints. `read_fd` is given to nom; `write_fd` is used by
/// build workers to forward stderr lines.
pub struct NomPipe {
    pub read_fd: OwnedFd,
    pub write_fd: OwnedFd,
}

impl NomPipe {
    pub fn new() -> Result<Self> {
        #[cfg(unix)]
        {
            let (r, w) = nix::unistd::pipe().context("create nom pipe")?;
            Ok(Self {
                read_fd: r,
                write_fd: w,
            })
        }
        #[cfg(not(unix))]
        {
            anyhow::bail!("nix-output-monitor pipe only supported on unix");
        }
    }
}

/// Spawn `nom --json` (via `nix shell nixpkgs#nix-output-monitor` fallback)
/// with `pipe.read_fd` wired to its stdin.
pub async fn spawn_nom(pipe: &NomPipe, opts: &Options) -> Result<Child> {
    let mut nom_cmd = nix_shell("nixpkgs#nix-output-monitor", "nom");
    nom_cmd.push("--json".into());
    let cmd = maybe_remote(nom_cmd, opts);

    let stdin: Stdio = {
        #[cfg(unix)]
        {
            // Duplicate so we can keep our own read_fd around for the
            // lifetime of the pipe (the child takes ownership of `dup`'d
            // copy).
            let dup = nix::unistd::dup(pipe.read_fd.as_raw_fd()).context("dup nom read fd")?;
            // SAFETY: `dup` returned a fresh fd we exclusively own.
            unsafe { Stdio::from_raw_fd(dup) }
        }
        #[cfg(not(unix))]
        {
            Stdio::null()
        }
    };

    let child = Command::new(&cmd[0])
        .args(&cmd[1..])
        .stdin(stdin)
        .spawn()
        .context("spawn nix-output-monitor")?;
    Ok(child)
}
