//! Pod-readiness wait helper.
//!
//! Bitnami helm charts surface `app.kubernetes.io/instance=<release>` on
//! every workload pod. The helm `--wait` flag only blocks until the
//! `Deployment` replica count is satisfied (and the firestream helm
//! lifecycle's `validate()` only checks pod `phase=Running`), neither
//! of which equals "the pod's containers report Ready". This module
//! adds the missing readiness check so Phase 3+ can synchronise probes
//! against actually-serving pods.

use std::path::Path;
use std::process::Command;
use std::time::Instant;

use anyhow::{Result, anyhow, bail};

/// Field selector applied alongside the instance label.
///
/// A pod that has already **terminated successfully** can never satisfy
/// `condition=Ready` — `Ready` goes false the moment the containers exit — so
/// including one in the wait set guarantees a timeout no matter how healthy the
/// rest of the release is.
///
/// This is not hypothetical. Bitnami's `common.labels.standard` is applied to
/// *Job* pod templates too, so a chart carrying a plain (non-hook) Job leaves a
/// `Completed` pod matching `app.kubernetes.io/instance=<release>` for the
/// lifetime of the namespace. Superset's `templates/init/init-job.yaml` is
/// exactly that, and it made every superset e2e run fail with:
///
/// ```text
/// timed out waiting for the condition on pods/superset-init-4h8fh
/// ```
///
/// even though all five workload pods were `1/1 Running`. Airflow escapes only
/// by accident: its migration Job is a Helm hook with
/// `hook-delete-policy: hook-succeeded`, so the pod is gone before we wait.
///
/// `Failed` is deliberately NOT excluded. A one-shot Job that crashed is a real
/// failure we want to surface, and letting the wait time out on it keeps the pod
/// around for the operator to inspect.
const NOT_SUCCEEDED: &str = "--field-selector=status.phase!=Succeeded";

/// Build the `kubectl wait` argv. Split out from [`wait_pods_ready`] so the
/// flag set can be asserted in tests without a cluster.
fn wait_argv(kubeconfig: &Path, release: &str, namespace: &str, remaining_secs: u64) -> Vec<String> {
    vec![
        "--kubeconfig".to_string(),
        kubeconfig.display().to_string(),
        "wait".to_string(),
        "--for=condition=Ready".to_string(),
        "pod".to_string(),
        "-l".to_string(),
        format!("app.kubernetes.io/instance={}", release),
        NOT_SUCCEEDED.to_string(),
        "-n".to_string(),
        namespace.to_string(),
        format!("--timeout={}s", remaining_secs),
    ]
}

/// Block until every pod with `app.kubernetes.io/instance=<release>` in
/// `namespace` reports `Ready`, or `deadline` elapses.
///
/// Shells out to:
///
/// ```text
/// kubectl --kubeconfig <kubeconfig> \
///   wait --for=condition=Ready pod \
///   -l app.kubernetes.io/instance=<release> \
///   --field-selector=status.phase!=Succeeded \
///   -n <namespace> \
///   --timeout=<remaining-secs>s
/// ```
///
/// See [`NOT_SUCCEEDED`] for why the field selector is load-bearing rather than
/// defensive.
///
/// We pass the remaining deadline (in whole seconds, minimum 1) into
/// `kubectl wait --timeout` so the kubectl client and the harness agree
/// on when to give up. Returns `Err` with stderr captured on failure.
pub fn wait_pods_ready(
    kubeconfig: &Path,
    release: &str,
    namespace: &str,
    deadline: Instant,
) -> Result<()> {
    let now = Instant::now();
    if now >= deadline {
        bail!(
            "wait_pods_ready: deadline already elapsed for release={} ns={}",
            release,
            namespace
        );
    }
    // Whole seconds, minimum 1 — kubectl rejects `--timeout=0s` outright
    // and we don't want a sub-second budget either.
    let remaining_secs = deadline.saturating_duration_since(now).as_secs().max(1);

    let output = Command::new("kubectl")
        .args(wait_argv(kubeconfig, release, namespace, remaining_secs))
        .output()
        .map_err(|e| anyhow!("spawn kubectl wait: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "kubectl wait failed (release={} ns={} status={}): stderr: {} stdout: {}",
            release,
            namespace,
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
            String::from_utf8_lossy(&output.stdout).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn argv() -> Vec<String> {
        wait_argv(&PathBuf::from("/tmp/kc"), "superset", "superset", 600)
    }

    /// The regression this module exists to prevent: without the field
    /// selector, a chart with a plain (non-hook) Job leaves a `Completed` pod
    /// carrying the instance label, and the wait can never succeed.
    #[test]
    fn wait_excludes_successfully_terminated_pods() {
        assert!(
            argv().contains(&"--field-selector=status.phase!=Succeeded".to_string()),
            "kubectl wait must exclude Succeeded pods; argv was {:?}",
            argv()
        );
    }

    /// `Failed` must NOT be excluded — a crashed one-shot Job is a real failure
    /// and the pod should stay around to be inspected.
    #[test]
    fn wait_does_not_exclude_failed_pods() {
        assert!(
            !argv().iter().any(|a| a.contains("status.phase!=Failed")),
            "Failed pods must still be waited on so crashes surface"
        );
    }

    #[test]
    fn wait_selects_on_the_release_instance_label() {
        assert!(
            argv().contains(&"app.kubernetes.io/instance=superset".to_string()),
            "argv was {:?}",
            argv()
        );
    }

    #[test]
    fn wait_passes_the_remaining_budget_as_the_kubectl_timeout() {
        assert!(argv().contains(&"--timeout=600s".to_string()));
    }
}
