//! `firestream-e2e-sweep`: remove resources that e2e harnesses left behind.
//!
//! ```text
//! firestream-e2e-sweep [--dry-run] [--run <id> [--target <target>]]
//! ```
//!
//! Without `--run` it removes every harness-owned resource on the host (see
//! `firestream_e2e_core::sweep` for the exact filters). It waits for the
//! host-wide e2e lock first, so it never races a running test.
//!
//! Exit codes: 0 clean (or dry run), 1 a step or listing failed, 2 usage.

use std::process::ExitCode;

use firestream_e2e_core::lock::HostLock;
use firestream_e2e_core::run_id::RunId;
use firestream_e2e_core::runner::SystemRunner;
use firestream_e2e_core::sweep::{Outcome, Scope, sweep};

const USAGE: &str = "usage: firestream-e2e-sweep [--dry-run] [--run <id> [--target <target>]]

  --dry-run          list what would be removed; remove nothing
  --run <id>         only resources of run <id> (firestream-e2e-<target>-<id>)
  --target <target>  with --run: only firestream-e2e-<target>-<id>";

struct Args {
    dry_run: bool,
    run: Option<RunId>,
    target: Option<String>,
}

fn parse_args() -> Result<Args, String> {
    let mut out = Args {
        dry_run: false,
        run: None,
        target: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" | "-n" => out.dry_run = true,
            "--run" => {
                let v = it.next().ok_or("--run needs a value")?;
                out.run = Some(RunId::parse(&v).map_err(|e| e.to_string())?);
            }
            "--target" => {
                let v = it.next().ok_or("--target needs a value")?;
                if !firestream_e2e_core::run_id::is_valid_target(&v) {
                    return Err(format!("invalid target {v:?}"));
                }
                out.target = Some(v);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if out.target.is_some() && out.run.is_none() {
        return Err("--target needs --run".into());
    }
    Ok(out)
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("firestream-e2e-sweep: {e}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let scope = match &args.run {
        Some(run) => Scope::for_run(run, args.target.as_deref()),
        None => Scope::All,
    };

    let lock = match HostLock::acquire() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("firestream-e2e-sweep: {e:#}");
            return ExitCode::from(1);
        }
    };

    let mode = if args.dry_run { " (dry run)" } else { "" };
    println!("firestream-e2e-sweep: {}{mode}", scope.describe());
    let (inv, actions, report) = sweep(&SystemRunner, &scope, &std::env::temp_dir(), &lock, args.dry_run);

    for w in &inv.warnings {
        println!("warning: {w}");
    }
    for e in &inv.errors {
        println!("error: {e}");
    }

    let objects = inv.objects();
    if objects.is_empty() {
        println!("No harness-owned resources found.");
    } else {
        println!("Found {} harness-owned resource(s):", objects.len());
        for o in &objects {
            println!("  {o}");
        }
    }

    if args.dry_run {
        if !actions.is_empty() {
            println!("Would run:");
            for a in &actions {
                println!("  {a}");
            }
        }
    } else {
        for (action, outcome) in &report.steps {
            match outcome {
                Outcome::Removed => println!("  ok      {action}"),
                Outcome::AlreadyGone => println!("  gone    {action}"),
                Outcome::Failed(why) => println!("  FAILED  {action}: {why}"),
            }
        }
    }

    if inv.errors.is_empty() && report.failures().is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
