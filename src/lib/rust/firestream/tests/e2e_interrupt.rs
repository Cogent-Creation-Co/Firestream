// SIGTERM during `docker compose up` leaves no object of the run.
//
//   make test-e2e-cleanup
//   cargo test -p firestream --test e2e_interrupt -- --ignored --test-threads=1 --nocapture
//
// This file is its own test binary, so `make test-e2e` does not run it. It
// loads the docker harness through `#[path]`, as `tests/e2e.rs` does.

#[path = "e2e/stacks.rs"]
#[allow(dead_code)]
mod stacks;
#[path = "e2e/probes.rs"]
#[allow(dead_code)]
mod probes;
#[path = "e2e/harness.rs"]
#[allow(dead_code)]
mod harness;

use firestream_e2e_core::interrupt;

// NOTE: redis is the smallest canonical stack, so a cold image build costs
// the least before `up` creates the first object.
const STACK: &str = "redis";

/// The child: a normal harness run of one stack. The parent kills it while
/// `nix run .#redis-up` creates the compose project.
#[test]
#[ignore = "e2e: child of e2e_sigterm_during_up; does nothing on its own"]
fn e2e_interrupt_child() {
    if !interrupt::is_child() {
        return;
    }
    harness::run_one(STACK);
}

#[test]
#[ignore = "e2e: needs nix+docker; run via `make test-e2e-cleanup`"]
fn e2e_sigterm_during_up() {
    if which::which("docker").is_err() || which::which("nix").is_err() {
        eprintln!("[e2e:interrupt] SKIP: docker or nix not on PATH");
        return;
    }
    let report = interrupt::run(
        "e2e_interrupt_child",
        STACK,
        &[("FIRESTREAM_E2E_STACKS", STACK), ("FIRESTREAM_E2E_STRICT", "1")],
    )
    .expect("interrupt run");
    eprintln!(
        "[e2e:interrupt] run {} signalled after {:?}, exit {}",
        report.run, report.signalled_after, report.status
    );
    report.assert_clean();
}
