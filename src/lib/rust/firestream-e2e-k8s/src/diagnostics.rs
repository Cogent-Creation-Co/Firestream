//! On-failure cluster diagnostics.
//!
//! When a chart fails to deploy or never becomes Ready, the harness tears the
//! whole k3d cluster down (`ClusterGuard`) and every artefact goes with it. The
//! failure then reaches the operator as one line — typically helm's
//! `context deadline exceeded` — with nothing to act on.
//!
//! This module dumps the state that actually explains such failures BEFORE the
//! teardown runs: pod status, the events feed, and container logs (including
//! `--previous`, which is where an OOMKilled or CrashLooping container's last
//! words live).
//!
//! Ordering matters and is why this exists as an explicit step rather than a
//! `Drop` impl: `--atomic` is disabled for e2e (see `deploy.rs`) precisely so
//! the workloads are still present when this runs. Measured on a live cluster,
//! an `--atomic` failure makes container logs unreachable before helm even
//! returns.

use std::path::Path;
use std::process::Command;

/// Cap per-container log output so one chatty pod cannot bury the rest.
const LOG_TAIL_LINES: &str = "80";

fn kubectl(kubeconfig: &Path, args: &[&str]) -> String {
    let out = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(kubeconfig)
        .args(args)
        .output();
    match out {
        Ok(o) => {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            let err = String::from_utf8_lossy(&o.stderr);
            if !err.trim().is_empty() {
                s.push_str(&format!("\n[stderr] {}", err.trim()));
            }
            s
        }
        Err(e) => format!("[kubectl spawn failed: {e}]"),
    }
}

/// Dump everything useful about `namespace` to stderr, labelled with `chart`.
///
/// Best-effort by construction: this runs on a path that is ALREADY failing, so
/// it never returns an error and never panics — a diagnostic helper that can
/// itself fail the test would be worse than none.
pub fn dump_namespace(chart: &str, kubeconfig: &Path, namespace: &str) {
    eprintln!("\n=== [e2e-k8s:{chart}] FAILURE DIAGNOSTICS (ns={namespace}) ===");

    eprintln!("--- pods ---");
    eprintln!("{}", kubectl(kubeconfig, &["get", "pods", "-n", namespace, "-o", "wide"]));

    // Container-level state: `lastState.terminated.reason` is where OOMKilled
    // and non-zero exit codes surface, and it is the field that identified the
    // superset worker failure.
    eprintln!("--- container states ---");
    eprintln!(
        "{}",
        kubectl(
            kubeconfig,
            &[
                "get", "pods", "-n", namespace,
                "-o",
                "jsonpath={range .items[*]}{.metadata.name}{\"\\n\"}\
                 {range .status.initContainerStatuses[*]}  init/{.name} ready={.ready} state={.state} last={.lastState}{\"\\n\"}{end}\
                 {range .status.containerStatuses[*]}  {.name} ready={.ready} restarts={.restartCount} state={.state} last={.lastState}{\"\\n\"}{end}{end}",
            ],
        )
    );

    // `describe` for pods that are not Ready. A Pending pod has NO
    // containerStatuses at all, so the jsonpath above prints just its name —
    // the actual reason (FailedScheduling, ImagePullBackOff, unbound PVC) is
    // only in the describe output's Events section. Observed on a forced
    // deploy timeout, where the jsonpath block came back empty for two Pending
    // pods and told the operator nothing.
    let not_ready = kubectl(
        kubeconfig,
        &[
            "get", "pods", "-n", namespace,
            "--field-selector", "status.phase!=Running",
            "-o", "jsonpath={.items[*].metadata.name}",
        ],
    );
    for pod in not_ready.split_whitespace().filter(|p| !p.starts_with('[')) {
        eprintln!("--- describe: {pod} (not Running) ---");
        // Trim to the tail: the Events section is at the bottom and is the part
        // that explains a stuck pod.
        let d = kubectl(kubeconfig, &["describe", "pod", pod, "-n", namespace]);
        let lines: Vec<&str> = d.lines().collect();
        let start = lines.len().saturating_sub(40);
        eprintln!("{}", lines[start..].join("\n"));
    }

    eprintln!("--- jobs ---");
    eprintln!("{}", kubectl(kubeconfig, &["get", "jobs", "-n", namespace]));

    eprintln!("--- warning events ---");
    eprintln!(
        "{}",
        kubectl(
            kubeconfig,
            &["get", "events", "-n", namespace, "--field-selector", "type=Warning",
              "--sort-by=.lastTimestamp"],
        )
    );

    // Logs last: the most valuable and the most voluminous.
    let pods = kubectl(
        kubeconfig,
        &["get", "pods", "-n", namespace, "-o", "jsonpath={.items[*].metadata.name}"],
    );
    for pod in pods.split_whitespace().filter(|p| !p.starts_with('[')) {
        eprintln!("--- logs: {pod} (all containers, tail {LOG_TAIL_LINES}) ---");
        eprintln!(
            "{}",
            kubectl(
                kubeconfig,
                &["logs", pod, "-n", namespace, "--all-containers=true",
                  "--tail", LOG_TAIL_LINES],
            )
        );
        // `--previous` is empty for a pod that never restarted; only print it
        // when there is something there, so the common case stays readable.
        let prev = kubectl(
            kubeconfig,
            &["logs", pod, "-n", namespace, "--all-containers=true",
              "--previous", "--tail", LOG_TAIL_LINES],
        );
        if !prev.trim().is_empty() && !prev.contains("[stderr]") {
            eprintln!("--- logs: {pod} (PREVIOUS instance — crash/OOM evidence) ---");
            eprintln!("{prev}");
        }
    }

    eprintln!("=== [e2e-k8s:{chart}] END DIAGNOSTICS ===\n");
}
