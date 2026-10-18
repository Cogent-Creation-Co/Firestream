//! CI step-summary writers for GitHub / Gitea / Forgejo Actions.
//! Mirrors `write_ci_summary()` + helpers at `__init__.py:1500-1685`.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use crate::display::strip_ansi;
use crate::result::{Outcome, ResultKind};

/// Returns the first set CI step-summary env var path.
pub fn ci_summary_file() -> Option<PathBuf> {
    for var in [
        "GITHUB_STEP_SUMMARY",
        "GITEA_STEP_SUMMARY",
        "FORGEJO_STEP_SUMMARY",
    ] {
        if let Ok(p) = std::env::var(var) {
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
    }
    None
}

pub fn write_ci_summary(path: &PathBuf, outcomes: &[Outcome], rc: u8) -> std::io::Result<()> {
    let (succ, fail) = group_by_kind(outcomes);
    let total_success: usize = succ.values().map(Vec::len).sum();
    let total_failed: usize = fail.values().map(Vec::len).sum();

    let mut lines = Vec::<String>::new();
    lines.push("# nix-fast-build Results\n".into());
    if rc == 0 {
        lines.push(format!(
            "## ✅ All Checks Passed ({total_success} successful)\n"
        ));
    } else {
        lines.push(format!(
            "## ❌ Build Failed ({total_failed} failed, {total_success} successful)\n"
        ));
    }

    format_failed_results(&fail, &mut lines);
    format_successful_results(&succ, &mut lines);

    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    f.write_all(lines.join("\n").as_bytes())?;
    Ok(())
}

type Bucket<'a> = BTreeMap<ResultKind, Vec<&'a Outcome>>;

fn group_by_kind(outcomes: &[Outcome]) -> (Bucket<'_>, Bucket<'_>) {
    let mut succ: Bucket = BTreeMap::new();
    let mut fail: Bucket = BTreeMap::new();
    for o in outcomes {
        if o.success {
            succ.entry(o.kind).or_default().push(o);
        } else {
            fail.entry(o.kind).or_default().push(o);
        }
    }
    (succ, fail)
}

fn format_failed_results(failed: &Bucket<'_>, lines: &mut Vec<String>) {
    if failed.is_empty() {
        return;
    }

    if let Some(evals) = failed.get(&ResultKind::Eval) {
        lines.push("\n### Failed Evaluations\n".into());
        for r in evals {
            lines.push(format!("**`{}`**\n", r.attr));
            if let Some(err) = &r.error {
                let err_lines: Vec<&str> = err.trim().split('\n').collect();
                if err_lines.len() > 3 {
                    lines.push("<details>".into());
                    lines.push("<summary>Error details</summary>\n".into());
                    lines.push("```".into());
                    for el in err_lines {
                        lines.push(el.to_string());
                    }
                    lines.push("```".into());
                    lines.push("</details>\n".into());
                } else {
                    lines.push(format!("Error: {err}\n"));
                }
            }
        }
    }

    if let Some(builds) = failed.get(&ResultKind::Build) {
        lines.push("\n### Failed Builds\n".into());
        for r in builds {
            lines.push(format!("\n**{}** (duration: {:.2}s)\n", r.attr, r.duration));
            if let Some(log) = &r.log_output {
                let stripped = strip_ansi(log);
                let log_lines: Vec<&str> = stripped.trim().split('\n').collect();
                let trimmed = if log_lines.len() > 100 {
                    let mut v = vec!["... (truncated, showing last 100 lines) ..."];
                    v.extend(&log_lines[log_lines.len() - 100..]);
                    v
                } else {
                    log_lines.clone()
                };
                lines.push("\n<details>".into());
                lines.push(format!(
                    "<summary>Build Log ({} lines)</summary>\n",
                    trimmed.len()
                ));
                lines.push("```".into());
                for l in trimmed {
                    lines.push(l.to_string());
                }
                lines.push("```".into());
                lines.push("</details>\n".into());
            } else if let Some(err) = &r.error {
                lines.push(format!("Error: {err}\n"));
            }
        }
    }

    for kind in [
        ResultKind::Upload,
        ResultKind::Download,
        ResultKind::Cachix,
        ResultKind::Attic,
        ResultKind::Niks3,
    ] {
        if let Some(rs) = failed.get(&kind) {
            lines.push(format!("\n### Failed {}s\n", kind.title_case()));
            for r in rs {
                lines.push(format!("**`{}`**\n", r.attr));
                if let Some(err) = &r.error {
                    lines.push(format!("Error: {err}\n"));
                }
            }
        }
    }
}

fn format_successful_results(succ: &Bucket<'_>, lines: &mut Vec<String>) {
    if succ.is_empty() {
        return;
    }
    lines.push("\n## Successful Operations\n".into());

    if let Some(builds) = succ.get(&ResultKind::Build) {
        lines.push("\n<details>".into());
        lines.push(format!(
            "<summary>Built {} packages</summary>\n",
            builds.len()
        ));
        for r in builds {
            lines.push(format!("- {} ({:.2}s)", r.attr, r.duration));
        }
        lines.push("</details>\n".into());
    }

    if let Some(evals) = succ.get(&ResultKind::Eval) {
        lines.push("\n<details>".into());
        lines.push(format!(
            "<summary>Evaluated {} attributes</summary>\n",
            evals.len()
        ));
        for r in evals {
            lines.push(format!("- {}", r.attr));
        }
        lines.push("</details>\n".into());
    }

    for kind in [
        ResultKind::Upload,
        ResultKind::Download,
        ResultKind::Cachix,
        ResultKind::Attic,
        ResultKind::Niks3,
    ] {
        if let Some(rs) = succ.get(&kind) {
            lines.push("\n<details>".into());
            lines.push(format!(
                "<summary>{}: {} successful</summary>\n",
                kind.title_case(),
                rs.len()
            ));
            for r in rs {
                lines.push(format!("- {}", r.attr));
            }
            lines.push("</details>\n".into());
        }
    }
}

