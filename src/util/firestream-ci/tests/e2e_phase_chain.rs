//! **The gate on the Phase 9 e2e phases: advisory, and serialised.**
//!
//! Phase 9 turned the two `#[ignore]`d e2e harnesses into declared pipeline
//! phases. Two properties of that shape have to hold, and neither can be
//! checked by running the harnesses (a cold sweep is measured in hours and
//! creates real k3d clusters):
//!
//! 1. **Advisory means exit 2, not 1.** A failing e2e phase must downgrade the
//!    verdict to `PartiallyPassed` and must not skip the phases behind it —
//!    that is the entire reason the plan says these can run in CI without
//!    blocking merges.
//! 2. **Chained means serialised.** `Pipeline::run` fans a phase's tasks out
//!    with `join_all` and no concurrency cap, so "many parallel tasks in one
//!    phase" would put N harness runs on one machine at once. The shape that
//!    avoids it is one shell task per phase with the phases chained through
//!    `depends_on`. [`max_concurrent_shell_phases`] is the invariant that
//!    proves the chain is real: **no two shell-task phases may be mutually
//!    independent in the DAG.**
//!
//! Everything below is checked against `tests/fixtures/e2e-chain.json` — a
//! *synthetic* project (`acmeforge`), not Firestream, for the same reason
//! `profile_fixtures.rs` uses one: these are properties of the schema and the
//! runner, not knowledge about any repository.
//!
//! When `FIRESTREAM_CI_PROFILE` is set (the Firestream dev shell exports it),
//! [`serialisation_invariant_holds_for_the_ambient_profile`] re-runs the
//! concurrency invariant against whatever real manifest is on hand. That is
//! the opportunistic anti-drift check: if someone later adds a second shell
//! task to an `e2e-*` phase, or forks the chain, it fails inside the dev shell
//! without src/util ever learning a Firestream phase name.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use firestream_ci::pipeline::{Phase, Pipeline, Task, Tier, VerdictOutcome};
use firestream_ci::profile::{self, Profile};

fn fixture() -> Profile {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/e2e-chain.json");
    profile::load(&p).expect("e2e-chain fixture must load + validate")
}

// ───────────────────────────────────────────────────────────────────────────
// The serialisation invariant
// ───────────────────────────────────────────────────────────────────────────

/// Transitive ancestors of every phase runnable in `mode`, mirroring the
/// runner: only phases that are *actually in this run* participate, and a
/// dependency filtered out by mode (or with no work) is treated as satisfied.
fn ancestor_sets(p: &Profile, mode: &str) -> BTreeMap<String, BTreeSet<String>> {
    let runnable: Vec<&str> = p
        .runnable_phases(mode)
        .into_iter()
        .map(|s| s.name.as_str())
        .collect();
    let live: BTreeSet<&str> = runnable.iter().copied().collect();

    let mut anc: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Process in topological order so a parent's ancestor set is already
    // complete when a child reads it.
    let order = p.phase_order(mode).expect("fixture DAG must be acyclic");
    for spec in order {
        if !live.contains(spec.name.as_str()) {
            continue;
        }
        let mut set: BTreeSet<String> = BTreeSet::new();
        for dep in &spec.depends_on {
            if !live.contains(dep.as_str()) {
                continue;
            }
            set.insert(dep.clone());
            if let Some(parent) = anc.get(dep) {
                set.extend(parent.iter().cloned());
            }
        }
        anc.insert(spec.name.clone(), set);
    }
    anc
}

/// Every unordered pair of shell-task phases that could run concurrently in
/// `mode` — i.e. neither is a transitive ancestor of the other. An empty
/// result is the property we want: the shell phases form a total order.
fn concurrent_shell_phase_pairs(p: &Profile, mode: &str) -> Vec<(String, String)> {
    let anc = ancestor_sets(p, mode);
    let shell: Vec<String> = p
        .runnable_phases(mode)
        .into_iter()
        .filter(|s| !s.shell_tasks.is_empty())
        .map(|s| s.name.clone())
        .collect();

    let mut out = Vec::new();
    for (i, a) in shell.iter().enumerate() {
        for b in shell.iter().skip(i + 1) {
            let a_before_b = anc.get(b).is_some_and(|s| s.contains(a));
            let b_before_a = anc.get(a).is_some_and(|s| s.contains(b));
            if !a_before_b && !b_before_a {
                out.push((a.clone(), b.clone()));
            }
        }
    }
    out
}

/// Peak number of harness processes a single phase can spawn at once. One is
/// the only safe answer for a phase that creates a k3d cluster or binds host
/// ports; `Pipeline::run` gives no way to cap it below "all of them".
fn max_concurrent_shell_phases(p: &Profile, mode: &str) -> usize {
    p.runnable_phases(mode)
        .into_iter()
        .map(|s| s.shell_tasks.len())
        .max()
        .unwrap_or(0)
}

// ───────────────────────────────────────────────────────────────────────────
// Gating: the e2e phases exist in exactly one mode
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn shell_phases_are_invisible_to_check_and_release() {
    let p = fixture();
    for mode in ["check", "release"] {
        let names: Vec<&str> = p
            .runnable_phases(mode)
            .into_iter()
            .map(|s| s.name.as_str())
            .collect();
        assert!(
            !names.iter().any(|n| n.starts_with("smoke-")),
            "mode={mode} must not materialise the e2e chain, got {names:?}"
        );
    }
}

#[test]
fn shell_phases_materialise_in_their_own_mode() {
    let p = fixture();
    let names: Vec<&str> = p
        .runnable_phases("e2e")
        .into_iter()
        .map(|s| s.name.as_str())
        .collect();
    // tidy + verify have no `modes`, so they run in every mode — the e2e
    // sweep still sits behind the normal required gate.
    assert!(names.contains(&"verify"), "got {names:?}");
    assert!(names.contains(&"smoke-alpha"), "got {names:?}");
    // ...but the release-only phase is still release-only.
    assert!(!names.contains(&"build"), "got {names:?}");
}

#[test]
fn every_shell_phase_is_advisory() {
    let p = fixture();
    for spec in p.runnable_phases("e2e") {
        if spec.shell_tasks.is_empty() {
            continue;
        }
        assert_eq!(
            spec.tier,
            Tier::Advisory,
            "shell phase `{}` must be advisory — a required e2e phase would \
             gate merges on an hours-long, cluster-creating sweep",
            spec.name
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Serialisation
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn shell_phases_are_a_total_order() {
    let p = fixture();
    let pairs = concurrent_shell_phase_pairs(&p, "e2e");
    assert!(
        pairs.is_empty(),
        "these shell phases can run CONCURRENTLY, which would put multiple \
         harness runs (k3d clusters / host ports) on one machine: {pairs:?}"
    );
}

#[test]
fn no_phase_holds_more_than_one_shell_task() {
    let p = fixture();
    assert_eq!(
        max_concurrent_shell_phases(&p, "e2e"),
        1,
        "a phase's tasks run under join_all with no concurrency cap; two \
         shell tasks in one phase is two harnesses at once"
    );
}

/// **The skip-propagation gotcha.**
///
/// `Pipeline::run` skips a phase only when one of its OWN `depends_on` is a
/// *required* phase that failed. It is not transitive, and it cannot be: a
/// skipped `PhaseOutcome` carries no tasks, so `ok()` — an `all()` over an
/// empty list — is `true`, and an advisory link's tier would not trigger a
/// skip even if it were false. A chain wired only link-to-link would
/// therefore skip its HEAD when `verify` goes red and then run every
/// remaining link anyway.
///
/// The fix is data, not Rust: every link names the required gate directly, in
/// addition to its predecessor. This test is the gate on that wiring.
#[test]
fn every_shell_phase_names_the_required_gate_directly() {
    let p = fixture();
    let required_gates: BTreeSet<&str> = p
        .runnable_phases("e2e")
        .into_iter()
        .filter(|s| s.tier == Tier::Required)
        .map(|s| s.name.as_str())
        .collect();
    assert!(!required_gates.is_empty(), "fixture must have a gate");

    for spec in p.runnable_phases("e2e") {
        if spec.shell_tasks.is_empty() {
            continue;
        }
        assert!(
            spec.depends_on.iter().any(|d| required_gates.contains(d.as_str())),
            "shell phase `{}` must name a required gate in its OWN depends_on \
             ({:?}) — skip does not propagate through an advisory link",
            spec.name,
            spec.depends_on
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// The exit-code proof: advisory failure is 2, not 1
// ───────────────────────────────────────────────────────────────────────────

/// Build the pipeline the runner would build for `mode`, but with every task
/// replaced by a canned result — `fail` names the phases whose task fails.
/// This is the same phase/tier/dependency wiring as
/// `run_ci_linux_profile_driven`, minus anything that touches a machine.
async fn run_with_failures(p: &Profile, mode: &str, fail: &[&str]) -> firestream_ci::pipeline::Verdict {
    let runnable: BTreeSet<String> = p
        .runnable_phases(mode)
        .into_iter()
        .map(|s| s.name.clone())
        .collect();
    let mut builder = Pipeline::builder();

    for spec in p.phase_order(mode).expect("acyclic") {
        if !runnable.contains(&spec.name) {
            continue;
        }
        let mut phase = Phase::new(spec.name.clone(), spec.tier);
        for dep in &spec.depends_on {
            if runnable.contains(dep) {
                phase = phase.depends_on(dep.clone());
            }
        }
        let name = spec.name.clone();
        let should_fail = fail.contains(&spec.name.as_str());
        builder = builder.phase(phase.parallel(move || {
            let name = name.clone();
            vec![Task::new(name.clone(), async move {
                if should_fail {
                    Err(format!("{name}: synthetic failure"))
                } else {
                    Ok(())
                }
            })]
        }));
    }

    builder.run().await.expect("pipeline must build")
}

#[tokio::test]
async fn a_failing_advisory_e2e_phase_yields_exit_2() {
    let p = fixture();
    let v = run_with_failures(&p, "e2e", &["smoke-alpha"]).await;
    assert_eq!(
        v.overall,
        VerdictOutcome::PartiallyPassed,
        "advisory failure must be PartiallyPassed, got {:?}",
        v.overall
    );
    assert_eq!(v.exit_code(), 2, "advisory failure must exit 2, not 1");
}

#[tokio::test]
async fn a_failing_e2e_phase_does_not_stop_the_sweep() {
    let p = fixture();
    let v = run_with_failures(&p, "e2e", &["smoke-alpha"]).await;
    // Only a failing REQUIRED upstream skips a dependent, so the rest of the
    // chain still runs — matching the cargo harness, where each chart is its
    // own `#[test]`.
    for name in ["smoke-beta", "smoke-gamma"] {
        let ph = v
            .phases
            .iter()
            .find(|o| o.name == name)
            .unwrap_or_else(|| panic!("{name} must appear in the verdict"));
        assert!(!ph.skipped, "{name} was skipped after an advisory failure");
        assert!(ph.ok(), "{name} should have passed");
    }
    assert_eq!(v.exit_code(), 2);
}

#[tokio::test]
async fn every_e2e_phase_failing_is_still_only_exit_2() {
    let p = fixture();
    let v = run_with_failures(&p, "e2e", &["smoke-alpha", "smoke-beta", "smoke-gamma"]).await;
    assert_eq!(v.exit_code(), 2, "a totally red e2e sweep must not gate");
}

#[tokio::test]
async fn a_failing_required_gate_skips_the_chain_and_exits_1() {
    let p = fixture();
    let v = run_with_failures(&p, "e2e", &["verify"]).await;
    assert_eq!(v.exit_code(), 1, "a red required phase must still exit 1");
    for name in ["smoke-alpha", "smoke-beta", "smoke-gamma"] {
        let ph = v.phases.iter().find(|o| o.name == name).expect("present");
        assert!(
            ph.skipped,
            "{name} must be skipped when the required gate is red — no point \
             burning cluster time on a broken tree"
        );
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Opportunistic anti-drift against whatever real profile is on hand
// ───────────────────────────────────────────────────────────────────────────

fn ambient_profile() -> Option<Profile> {
    let raw = std::env::var("FIRESTREAM_CI_PROFILE").ok()?;
    if raw.is_empty() {
        return None;
    }
    let p = PathBuf::from(raw);
    let file = if p.is_dir() {
        p.join("ci-manifest.json")
    } else {
        p
    };
    if !file.exists() {
        return None;
    }
    profile::load(&file).ok()
}

/// Structural, name-free: for EVERY mode the ambient manifest mentions, the
/// shell-task phases must be a total order and no phase may hold more than one
/// shell task. Skips silently when no manifest is available (plain
/// `cargo test` outside the dev shell).
#[test]
fn serialisation_invariant_holds_for_the_ambient_profile() {
    let Some(p) = ambient_profile() else {
        eprintln!("SKIP: FIRESTREAM_CI_PROFILE unset or unreadable");
        return;
    };
    let mut modes: BTreeSet<String> = ["check", "release"].iter().map(|s| s.to_string()).collect();
    for ph in &p.phases {
        modes.extend(ph.modes.iter().cloned());
    }
    for mode in &modes {
        let pairs = concurrent_shell_phase_pairs(&p, mode);
        assert!(
            pairs.is_empty(),
            "mode={mode}: shell phases can run concurrently: {pairs:?}"
        );
        let max = max_concurrent_shell_phases(&p, mode);
        assert!(
            max <= 1,
            "mode={mode}: a phase declares {max} shell tasks; they would run \
             under join_all with no concurrency cap"
        );

        // Skip is not transitive — see `every_shell_phase_names_the_required
        // _gate_directly`. If this mode has any required phase at all, every
        // shell phase must name one directly.
        let gates: BTreeSet<&str> = p
            .runnable_phases(mode)
            .into_iter()
            .filter(|s| s.tier == Tier::Required)
            .map(|s| s.name.as_str())
            .collect();
        if gates.is_empty() {
            continue;
        }
        for spec in p.runnable_phases(mode) {
            if spec.shell_tasks.is_empty() {
                continue;
            }
            assert!(
                spec.depends_on.iter().any(|d| gates.contains(d.as_str())),
                "mode={mode}: shell phase `{}` does not name a required gate \
                 in its own depends_on ({:?}); a red gate would skip only the \
                 head of the chain",
                spec.name,
                spec.depends_on
            );
        }
    }
}

/// Also name-free: any phase that shells out must be advisory. A required
/// shell phase is a machine-dependent gate on the merge path.
#[test]
fn ambient_shell_phases_are_advisory() {
    let Some(p) = ambient_profile() else {
        eprintln!("SKIP: FIRESTREAM_CI_PROFILE unset or unreadable");
        return;
    };
    for ph in &p.phases {
        if ph.shell_tasks.is_empty() {
            continue;
        }
        assert_eq!(
            ph.tier,
            Tier::Advisory,
            "shell phase `{}` must be advisory",
            ph.name
        );
    }
}
