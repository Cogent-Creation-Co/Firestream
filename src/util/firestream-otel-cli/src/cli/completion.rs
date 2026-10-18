//! `otel-cli completion` — emit shell completion scripts.
//!
//! Go reference: `otelcli/completion.go`.

use std::io;

use anyhow::Result;
use clap::{Args, CommandFactory, ValueEnum};
use clap_complete::{generate, Shell};

#[derive(Debug, Args)]
pub struct CompletionArgs {
    #[arg(value_enum)]
    pub shell: ShellKind,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ShellKind {
    Bash,
    Zsh,
    Fish,
    Powershell,
}

impl From<ShellKind> for Shell {
    fn from(s: ShellKind) -> Shell {
        match s {
            ShellKind::Bash => Shell::Bash,
            ShellKind::Zsh => Shell::Zsh,
            ShellKind::Fish => Shell::Fish,
            ShellKind::Powershell => Shell::PowerShell,
        }
    }
}

pub fn run(args: CompletionArgs) -> Result<u8> {
    let mut cmd = super::Cli::command();
    let name = cmd.get_name().to_string();
    generate(Shell::from(args.shell), &mut cmd, name, &mut io::stdout());
    Ok(0)
}
