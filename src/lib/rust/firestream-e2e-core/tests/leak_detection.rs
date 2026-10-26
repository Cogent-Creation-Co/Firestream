//! Proves that the leak check sees a real leftover, and that the sweep
//! removes it.
//!
//! The test plants two docker objects that carry a fresh run's names and
//! labels: a k3d-style network and a compose-style volume. Both must make
//! `leak::check` fail; after `firestream-e2e-sweep --run <id>` (here the
//! library call) the check must pass.
//!
//! ```text
//! cargo test -p firestream-e2e-core --test leak_detection -- --ignored --nocapture
//! ```

use std::process::Command;

use firestream_e2e_core::leak;
use firestream_e2e_core::lock::HostLock;
use firestream_e2e_core::run_id::RunId;
use firestream_e2e_core::runner::SystemRunner;
use firestream_e2e_core::sweep::{Scope, sweep};

const TARGET: &str = "leakprobe";

fn docker(args: &[&str]) -> bool {
    Command::new("docker")
        .args(args)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Removes the planted objects if the test fails before the sweep does.
struct Planted {
    network: String,
    volume: String,
}

impl Drop for Planted {
    fn drop(&mut self) {
        let _ = docker(&["network", "rm", &self.network]);
        let _ = docker(&["volume", "rm", "-f", &self.volume]);
    }
}

#[test]
#[ignore = "e2e: needs docker; run via `make test-e2e-cleanup`"]
fn planted_resources_fail_the_leak_check_until_swept() {
    if !docker(&["info", "--format", "{{.ID}}"]) {
        eprintln!("[leak-detection] SKIP: docker daemon not reachable");
        return;
    }
    let lock = HostLock::acquire().expect("acquire host e2e lock");
    let run = RunId::random();
    let project = run.resource_name(TARGET);
    let planted = Planted {
        network: format!("k3d-{project}"),
        volume: format!("{project}_data"),
    };

    assert!(
        docker(&["network", "create", "--label", "app=k3d", &planted.network]),
        "create network {}",
        planted.network
    );
    let project_label = format!("com.docker.compose.project={project}");
    assert!(
        docker(&["volume", "create", "--label", &project_label, &planted.volume]),
        "create volume {}",
        planted.volume
    );

    for target in [Some(TARGET), None] {
        let leaks = leak::check(&run, target, false).expect_err("planted objects must fail the check");
        let text = leaks.to_string();
        assert!(text.contains(&planted.network), "network not reported: {text}");
        assert!(text.contains(&planted.volume), "volume not reported: {text}");
        assert!(text.contains(&format!("make e2e-sweep RUN={run}")), "{text}");
    }

    assert!(
        leak::check(&run, Some("othertarget"), false).is_ok(),
        "a check for another target of the run must not see these objects"
    );
    assert!(leak::check(&run, None, true).is_ok(), "KEEP=1 skips the check");

    let (_, _, report) = sweep(&SystemRunner, &Scope::for_run(&run, None), &std::env::temp_dir(), &lock, false);
    assert!(report.failures().is_empty(), "sweep failed: {:?}", report.failures());

    leak::check(&run, None, false).unwrap_or_else(|l| panic!("objects remain after the sweep: {l}"));
}
