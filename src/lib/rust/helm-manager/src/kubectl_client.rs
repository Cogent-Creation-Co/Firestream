use std::path::PathBuf;
use std::process::Stdio;
use tokio::process::Command;

use crate::{Error, Result};

/// Kubectl CLI wrapper
pub struct KubectlClient {
    /// Path to kubectl binary
    kubectl_path: PathBuf,
    /// Kubeconfig path
    kubeconfig: Option<PathBuf>,
}

impl KubectlClient {
    /// Create a new KubectlClient
    pub fn new() -> Result<Self> {
        let kubectl_path = which::which("kubectl")
            .map_err(|_| Error::KubectlNotFound)?;

        Ok(Self {
            kubectl_path,
            kubeconfig: None,
        })
    }

    /// Create a new KubectlClient with custom configuration
    pub fn with_config(kubectl_path: Option<PathBuf>, kubeconfig: Option<PathBuf>) -> Result<Self> {
        let kubectl_path = if let Some(path) = kubectl_path {
            path
        } else {
            which::which("kubectl").map_err(|_| Error::KubectlNotFound)?
        };

        Ok(Self {
            kubectl_path,
            kubeconfig,
        })
    }

    /// Check if a namespace exists
    pub async fn namespace_exists(&self, name: &str) -> Result<bool> {
        let mut cmd = self.base_command();
        cmd.arg("get")
            .arg("namespace")
            .arg(name);

        let output = cmd.output().await?;
        Ok(output.status.success())
    }

    /// Create a namespace
    pub async fn create_namespace(&self, name: &str) -> Result<()> {
        // Check if namespace already exists
        if self.namespace_exists(name).await? {
            return Ok(());
        }

        let mut cmd = self.base_command();
        cmd.arg("create")
            .arg("namespace")
            .arg(name);

        let output = cmd.output().await?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::KubectlCommandFailed(format!(
                "Failed to create namespace: {}",
                stderr
            )));
        }

        Ok(())
    }

    /// Get pods for a release
    pub async fn get_pods(&self, namespace: &str, selector: &str) -> Result<Vec<String>> {
        let mut cmd = self.base_command();
        cmd.arg("get")
            .arg("pods")
            .arg("-n").arg(namespace)
            .arg("-l").arg(selector)
            .arg("-o").arg("jsonpath={.items[*].metadata.name}");

        let output = cmd.output().await?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::KubectlCommandFailed(format!(
                "Failed to get pods: {}",
                stderr
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let pods: Vec<String> = stdout
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        Ok(pods)
    }

    /// Wait for pods to be ready
    pub async fn wait_for_ready(&self, namespace: &str, selector: &str, timeout: u64) -> Result<()> {
        let mut cmd = self.base_command();
        cmd.arg("wait")
            .arg("--for=condition=ready")
            .arg("pod")
            .arg("-n").arg(namespace)
            .arg("-l").arg(selector)
            .arg(format!("--timeout={}s", timeout));

        let output = cmd.output().await?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::KubectlCommandFailed(format!(
                "Pods failed to become ready: {}",
                stderr
            )));
        }

        Ok(())
    }

    /// Get resource YAML
    pub async fn get_resource_yaml(&self, kind: &str, name: &str, namespace: &str) -> Result<String> {
        let mut cmd = self.base_command();
        cmd.arg("get")
            .arg(kind)
            .arg(name)
            .arg("-n").arg(namespace)
            .arg("-o").arg("yaml");

        let output = cmd.output().await?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::KubectlCommandFailed(format!(
                "Failed to get resource: {}",
                stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Get a resource as JSON (`kubectl get <kind> <name> -n <ns> -o json`).
    ///
    /// Unlike `-o jsonpath`, this emits valid JSON for nested structures
    /// (env/volumes/securityContext), so callers can `serde_json`-parse it and
    /// re-use sub-objects verbatim. Mirrors [`Self::get_resource_yaml`].
    pub async fn get_resource_json(&self, kind: &str, name: &str, namespace: &str) -> Result<String> {
        let mut cmd = self.base_command();
        cmd.arg("get")
            .arg(kind)
            .arg(name)
            .arg("-n").arg(namespace)
            .arg("-o").arg("json");

        let output = cmd.output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::KubectlCommandFailed(format!(
                "Failed to get resource: {}",
                stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Apply a resource from YAML
    pub async fn apply_yaml(&self, yaml: &str, namespace: Option<&str>) -> Result<()> {
        let mut cmd = self.base_command();
        cmd.arg("apply")
            .arg("-f")
            .arg("-");
        
        if let Some(ns) = namespace {
            cmd.arg("-n").arg(ns);
        }

        cmd.stdin(Stdio::piped());

        let mut child = cmd.spawn()?;
        
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(yaml.as_bytes()).await?;
        }

        let output = child.wait_with_output().await?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::KubectlCommandFailed(format!(
                "Failed to apply resource: {}",
                stderr
            )));
        }

        Ok(())
    }

    /// Delete a resource
    pub async fn delete_resource(&self, kind: &str, name: &str, namespace: &str) -> Result<()> {
        let mut cmd = self.base_command();
        cmd.arg("delete")
            .arg(kind)
            .arg(name)
            .arg("-n").arg(namespace);

        let output = cmd.output().await?;
        
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Ignore if resource not found
            if !stderr.contains("NotFound") {
                return Err(Error::KubectlCommandFailed(format!(
                    "Failed to delete resource: {}",
                    stderr
                )));
            }
        }

        Ok(())
    }

    /// Create a one-shot Job from an existing CronJob's job template.
    ///
    /// Shells out to
    /// `kubectl create job <job_name> --from=cronjob/<cronjob_name> -n <ns>`.
    /// The created Job reuses the CronJob's pod spec verbatim (image, env,
    /// volumes, command), which is exactly what we want for an on-demand
    /// backup that mirrors the scheduled one.
    pub async fn create_job_from_cronjob(
        &self,
        namespace: &str,
        cronjob_name: &str,
        job_name: &str,
    ) -> Result<()> {
        let mut cmd = self.base_command();
        cmd.arg("create")
            .arg("job")
            .arg(job_name)
            .arg(format!("--from=cronjob/{}", cronjob_name))
            .arg("-n").arg(namespace);

        let output = cmd.output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::KubectlCommandFailed(format!(
                "Failed to create job '{}' from cronjob '{}': {}",
                job_name, cronjob_name, stderr
            )));
        }

        Ok(())
    }

    /// Wait for a Job to finish, returning `Ok(())` on success and an error if
    /// the Job fails or the timeout elapses.
    ///
    /// `kubectl wait` can only block on a single condition, so we race a
    /// `condition=complete` wait against a `condition=failed` wait and surface
    /// whichever fires first. This means a failed Job is reported promptly
    /// instead of stalling until the timeout.
    pub async fn wait_for_job(&self, namespace: &str, job_name: &str, timeout: u64) -> Result<()> {
        let complete = self.run_job_wait(namespace, job_name, "complete", timeout);
        let failed = self.run_job_wait(namespace, job_name, "failed", timeout);

        tokio::select! {
            res = complete => match res? {
                true => Ok(()),
                false => Err(Error::KubectlCommandFailed(format!(
                    "Timed out after {}s waiting for job '{}' to complete",
                    timeout, job_name
                ))),
            },
            res = failed => match res? {
                true => Err(Error::KubectlCommandFailed(format!(
                    "Job '{}' failed before completing",
                    job_name
                ))),
                false => Err(Error::KubectlCommandFailed(format!(
                    "Timed out after {}s waiting for job '{}'",
                    timeout, job_name
                ))),
            },
        }
    }

    /// Run a single `kubectl wait --for=condition=<condition> job/<name>`.
    ///
    /// Returns `Ok(true)` if the condition was met, `Ok(false)` if the wait
    /// timed out (condition not yet met), and `Err` for any other failure.
    async fn run_job_wait(
        &self,
        namespace: &str,
        job_name: &str,
        condition: &str,
        timeout: u64,
    ) -> Result<bool> {
        let mut cmd = self.base_command();
        cmd.arg("wait")
            .arg(format!("--for=condition={}", condition))
            .arg(format!("job/{}", job_name))
            .arg("-n").arg(namespace)
            .arg(format!("--timeout={}s", timeout));

        let output = cmd.output().await?;
        if output.status.success() {
            return Ok(true);
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        // kubectl prints "timed out waiting for the condition" on timeout.
        if stderr.contains("timed out") || stderr.contains("timeout") {
            Ok(false)
        } else {
            Err(Error::KubectlCommandFailed(format!(
                "kubectl wait (condition={}) for job '{}' failed: {}",
                condition, job_name, stderr
            )))
        }
    }

    /// Fetch logs for a resource (e.g. `job/<name>`) in a namespace.
    pub async fn get_logs(&self, namespace: &str, resource: &str) -> Result<String> {
        let mut cmd = self.base_command();
        cmd.arg("logs")
            .arg(resource)
            .arg("-n").arg(namespace);

        let output = cmd.output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::KubectlCommandFailed(format!(
                "Failed to get logs for '{}': {}",
                resource, stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Read a single `jsonpath` field from a named resource.
    ///
    /// Returns the trimmed stdout (empty string if the path resolves to
    /// nothing). Errors only when the `kubectl get` itself fails.
    pub async fn get_jsonpath(
        &self,
        kind: &str,
        name: &str,
        namespace: &str,
        jsonpath: &str,
    ) -> Result<String> {
        let mut cmd = self.base_command();
        cmd.arg("get")
            .arg(kind)
            .arg(name)
            .arg("-n").arg(namespace)
            .arg("-o").arg(format!("jsonpath={}", jsonpath));

        let output = cmd.output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::KubectlCommandFailed(format!(
                "Failed to read jsonpath '{}' from {}/{}: {}",
                jsonpath, kind, name, stderr
            )));
        }

        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    /// Scale a workload (`kubectl scale <kind>/<name> --replicas=<n>`).
    pub async fn scale(&self, kind: &str, name: &str, namespace: &str, replicas: u32) -> Result<()> {
        let mut cmd = self.base_command();
        cmd.arg("scale")
            .arg(format!("{}/{}", kind, name))
            .arg(format!("--replicas={}", replicas))
            .arg("-n").arg(namespace);

        let output = cmd.output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::KubectlCommandFailed(format!(
                "Failed to scale {}/{} to {}: {}",
                kind, name, replicas, stderr
            )));
        }

        Ok(())
    }

    /// Block until every pod matching `selector` is gone
    /// (`kubectl wait --for=delete pod -l <selector>`). Returns `Ok(())`
    /// immediately when no pod matches.
    pub async fn wait_for_pods_deleted(&self, namespace: &str, selector: &str, timeout: u64) -> Result<()> {
        if self.get_pods(namespace, selector).await?.is_empty() {
            return Ok(());
        }

        let mut cmd = self.base_command();
        cmd.arg("wait")
            .arg("--for=delete")
            .arg("pod")
            .arg("-n").arg(namespace)
            .arg("-l").arg(selector)
            .arg(format!("--timeout={}s", timeout));

        let output = cmd.output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::KubectlCommandFailed(format!(
                "Pods matching '{}' were not deleted: {}",
                selector, stderr
            )));
        }

        Ok(())
    }

    /// Block until a rollout finishes (`kubectl rollout status <kind>/<name>`).
    pub async fn rollout_status(&self, kind: &str, name: &str, namespace: &str, timeout: u64) -> Result<()> {
        let mut cmd = self.base_command();
        cmd.arg("rollout")
            .arg("status")
            .arg(format!("{}/{}", kind, name))
            .arg("-n").arg(namespace)
            .arg(format!("--timeout={}s", timeout));

        let output = cmd.output().await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::KubectlCommandFailed(format!(
                "Rollout of {}/{} did not finish: {}",
                kind, name, stderr
            )));
        }

        Ok(())
    }

    /// Create base command with common arguments
    fn base_command(&self) -> Command {
        let mut cmd = Command::new(&self.kubectl_path);
        
        if let Some(ref kubeconfig) = self.kubeconfig {
            cmd.env("KUBECONFIG", kubeconfig);
        }
        
        cmd.kill_on_drop(true);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_kubectl_client_creation() {
        // This will fail if kubectl is not installed
        let result = KubectlClient::new();
        if which::which("kubectl").is_ok() {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err());
        }
    }
}