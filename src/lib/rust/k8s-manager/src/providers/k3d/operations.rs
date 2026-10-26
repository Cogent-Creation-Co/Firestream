//! Core cluster operations for K3D

use crate::{K8sManagerError, Result, K3dClusterConfig, K3dConfig, ClusterInfo, ClusterProvider, ClusterStatus};
use crate::providers::k3d::manager::K3dClusterManager;
use crate::providers::k3d::utils::get_api_endpoint;
use tokio::process::Command;
use tokio::fs;
use tokio::time::{sleep, Duration, timeout};
use tracing::{info, debug, warn};
use std::collections::HashMap;

impl K3dClusterManager {
    /// Full cluster setup matching bootstrap.sh functionality
    pub async fn setup_cluster(&self) -> Result<()> {
        info!("Setting up K3D cluster '{}'", self.config.name);
        
        // Ensure .kube directory exists
        self.ensure_kube_directory().await?;
        
        // IMPORTANT: Patch /etc/hosts FIRST to avoid DNS resolution issues with sudo
        if self.config.network.patch_etc_hosts {
            self.patch_etc_hosts().await?;
        }
        
        // Create registry if enabled
        if self.config.registry.enabled {
            self.setup_registry().await?;
        }
        
        // Create or reconnect cluster
        if self.cluster_exists().await? {
            info!("Cluster '{}' already exists, reconnecting...", self.config.name);
            self.reconnect_cluster().await?;
        } else {
            info!("Creating new cluster '{}'", self.config.name);
            self.create_cluster().await?;
            self.wait_for_cluster().await?;
            self.verify_cluster_health().await?;
        }
        
        // Configure network routes first (needed for kubectl commands in containers)
        if self.config.network.configure_routes {
            self.setup_routes().await?;
        }
        
        // Configure TLS after routes are established
        if self.config.tls.enabled {
            self.configure_tls().await?;
        }
        
        if self.config.network.configure_dns {
            self.configure_dns().await?;
        }
        
        // Test DNS resolution with retries
        self.test_dns_resolution().await?;
        
        // Setup dev mode if enabled
        if let Some(dev_config) = &self.config.dev_mode {
            if dev_config.port_forward_all {
                self.setup_port_forwarding(dev_config).await?;
            }
        }
        
        info!("K3D cluster '{}' setup complete", self.config.name);
        Ok(())
    }
    
    /// Ensure .kube directory exists
    pub(crate) async fn ensure_kube_directory(&self) -> Result<()> {
        let kube_dir = dirs::home_dir()
            .ok_or_else(|| K8sManagerError::GeneralError("Cannot find home directory".into()))?
            .join(".kube");
        
        fs::create_dir_all(&kube_dir).await
            .map_err(|e| K8sManagerError::IoError(format!("Failed to create .kube directory: {}", e)))?;
        
        Ok(())
    }
    
    /// Reconnect to existing cluster
    pub(crate) async fn reconnect_cluster(&self) -> Result<()> {
        info!("Reconnecting to cluster '{}'", self.config.name);
        
        // Write kubeconfig
        let output = Command::new("k3d")
            .args(&[
                "kubeconfig",
                "write",
                &self.config.name,
            ])
            .output()
            .await?;
        
        if !output.status.success() {
            return Err(K8sManagerError::GeneralError("Failed to write kubeconfig".to_string()));
        }
        
        // The kubeconfig path is printed to stdout
        let kubeconfig_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        info!("Kubeconfig written to: {}", kubeconfig_path);
        
        // Fix the kubeconfig if we're in a container
        self.fix_kubeconfig_for_container().await?;
        
        // Set context using the kubeconfig path
        let status = Command::new("kubectl")
            .args(&[
                "config",
                "use-context",
                &format!("k3d-{}", self.config.name),
            ])
            .env("KUBECONFIG", &kubeconfig_path)
            .status()
            .await?;
        
        if !status.success() {
            warn!("Failed to set kubectl context");
        }
        
        // Test kubectl connection
        self.test_kubectl().await?;
        
        Ok(())
    }
    
    /// Wait for cluster to be ready with comprehensive checks
    pub(crate) async fn wait_for_cluster(&self) -> Result<()> {
        info!("Waiting for cluster to be ready...");
        
        // Step 1: Wait for all nodes to be ready
        self.wait_for_nodes_ready().await?;
        
        // Step 2: Wait for critical system pods
        self.wait_for_system_pods().await?;
        
        // Step 3: Wait for CoreDNS specifically (critical for cluster functionality)
        self.wait_for_coredns().await?;
        
        info!("Cluster is ready!");
        Ok(())
    }
    
    /// Wait for all nodes to report ready status
    pub(crate) async fn wait_for_nodes_ready(&self) -> Result<()> {
        info!("Waiting for nodes to be ready...");
        
        let operation = "node readiness check";
        let timeout_duration = Duration::from_secs(self.config.timeouts.node_ready);
        
        timeout(timeout_duration, async {
            self.retry_with_backoff(operation, || async {
                let output = Command::new("kubectl")
                    .args(&["get", "nodes", "-o", "jsonpath={.items[*].status.conditions[?(@.type=='Ready')].status}"])
                    .output()
                    .await?;
                
                if !output.status.success() {
                    return Err(K8sManagerError::GeneralError("Failed to get node status".to_string()));
                }
                
                let statuses = String::from_utf8_lossy(&output.stdout);
                let all_ready = statuses.split_whitespace().all(|s| s == "True");
                
                if !all_ready {
                    return Err(K8sManagerError::GeneralError("Not all nodes are ready".to_string()));
                }
                
                // Also check that we have the expected number of nodes
                let node_count_output = Command::new("kubectl")
                    .args(&["get", "nodes", "-o", "jsonpath={.items[*].metadata.name}"])
                    .output()
                    .await?;
                
                if node_count_output.status.success() {
                    let nodes = String::from_utf8_lossy(&node_count_output.stdout);
                    let actual_count = nodes.split_whitespace().count() as u32;
                    let expected_count = self.config.servers + self.config.agents;
                    
                    if actual_count != expected_count {
                        return Err(K8sManagerError::GeneralError(
                            format!("Expected {} nodes, found {}", expected_count, actual_count)
                        ));
                    }
                }
                
                info!("All nodes are ready");
                Ok(())
            }).await
        }).await
        .map_err(|_| K8sManagerError::Timeout(format!("{} timed out after {:?}", operation, timeout_duration)))?
    }
    
    /// Wait for critical system pods to be running
    pub(crate) async fn wait_for_system_pods(&self) -> Result<()> {
        info!("Waiting for system pods to be ready...");
        
        let critical_deployments = vec![
            "coredns",
            "local-path-provisioner",
            "metrics-server", // May not always be present
        ];
        
        let timeout_duration = Duration::from_secs(self.config.timeouts.cluster_ready);
        
        for deployment in critical_deployments {
            let operation = format!("waiting for {}", deployment);
            
            // Check if deployment exists first
            let check_output = Command::new("kubectl")
                .args(&[
                    "get", "deployment", deployment,
                    "-n", "kube-system",
                    "--no-headers"
                ])
                .output()
                .await?;
            
            if !check_output.status.success() {
                debug!("{} deployment not found, skipping", deployment);
                continue;
            }
            
            // Wait for deployment to be ready
            let result = timeout(timeout_duration, async {
                self.retry_with_backoff(&operation, || async {
                    let status = Command::new("kubectl")
                        .args(&[
                            "wait",
                            "--namespace=kube-system",
                            "--for=condition=available",
                            &format!("--timeout={}s", self.config.timeouts.node_ready),
                            &format!("deployment/{}", deployment),
                        ])
                        .status()
                        .await?;
                    
                    if !status.success() {
                        return Err(K8sManagerError::GeneralError(
                            format!("{} is not yet available", deployment)
                        ));
                    }
                    
                    Ok(())
                }).await
            }).await;
            
            match result {
                Ok(_) => debug!("{} is ready", deployment),
                Err(_) if deployment == "coredns" => {
                    // CoreDNS is critical, fail if it's not ready
                    return Err(K8sManagerError::Timeout(
                        format!("CoreDNS deployment failed to become ready within {:?}", timeout_duration)
                    ));
                }
                Err(e) => {
                    // Other deployments are optional, just warn
                    warn!("Optional deployment {} not ready: {}", deployment, e);
                }
            }
        }
        
        Ok(())
    }
    
    /// Wait specifically for CoreDNS to be fully functional
    pub(crate) async fn wait_for_coredns(&self) -> Result<()> {
        info!("Verifying CoreDNS functionality...");
        
        let timeout_duration = Duration::from_secs(self.config.timeouts.dns_check);
        
        timeout(timeout_duration, async {
            self.retry_with_backoff("CoreDNS functionality check", || async {
                // Check if CoreDNS pods are running
                let output = Command::new("kubectl")
                    .args(&[
                        "get", "pods",
                        "-n", "kube-system",
                        "-l", "k8s-app=kube-dns",
                        "-o", "jsonpath={.items[*].status.phase}"
                    ])
                    .output()
                    .await?;
                
                if !output.status.success() {
                    return Err(K8sManagerError::GeneralError("Failed to check CoreDNS pods".to_string()));
                }
                
                let phases = String::from_utf8_lossy(&output.stdout);
                let all_running = phases.split_whitespace().all(|p| p == "Running");
                
                if !all_running {
                    return Err(K8sManagerError::GeneralError("CoreDNS pods are not all running".to_string()));
                }
                
                // Test DNS resolution within the cluster
                let dns_test = Command::new("kubectl")
                    .args(&[
                        "run", "dns-test",
                        "--rm", "-i", "--restart=Never",
                        "--image=busybox:1.28",
                        "--",
                        "nslookup", "kubernetes.default"
                    ])
                    .output()
                    .await?;
                
                if !dns_test.status.success() {
                    return Err(K8sManagerError::DnsError("DNS resolution test failed".to_string()));
                }
                
                info!("CoreDNS is fully functional");
                Ok(())
            }).await
        }).await
        .map_err(|_| K8sManagerError::Timeout(format!("CoreDNS verification timed out after {:?}", timeout_duration)))?
    }
    
    /// Test kubectl connection
    pub(crate) async fn test_kubectl(&self) -> Result<()> {
        let output = Command::new("kubectl")
            .args(&["version", "--output=json"])
            .output()
            .await?;
        
        if !output.status.success() {
            return Err(K8sManagerError::GeneralError("kubectl connection failed".to_string()));
        }
        
        let version_json = String::from_utf8_lossy(&output.stdout);
        let version: serde_json::Value = serde_json::from_str(&version_json)
            .map_err(|e| K8sManagerError::GeneralError(format!("Failed to parse kubectl version: {}", e)))?;
        
        if version["clientVersion"]["gitVersion"].is_null() || 
           version["serverVersion"]["gitVersion"].is_null() {
            return Err(K8sManagerError::GeneralError("kubectl version check failed".to_string()));
        }
        
        info!("kubectl connection verified");
        Ok(())
    }
    
    /// Comprehensive cluster health verification
    pub(crate) async fn verify_cluster_health(&self) -> Result<()> {
        info!("Verifying cluster health...");
        
        // 1. Verify API server is responsive
        self.retry_with_backoff("API server health check", || async {
            let output = Command::new("kubectl")
                .args(&["cluster-info"])
                .output()
                .await?;
            
            if !output.status.success() {
                return Err(K8sManagerError::GeneralError("API server is not responsive".to_string()));
            }
            
            Ok(())
        }).await?;
        
        // 2. Verify all expected components are present
        let expected_namespaces = vec!["default", "kube-system", "kube-public", "kube-node-lease"];
        for ns in expected_namespaces {
            let output = Command::new("kubectl")
                .args(&["get", "namespace", ns])
                .output()
                .await?;
            
            if !output.status.success() {
                return Err(K8sManagerError::GeneralError(
                    format!("Expected namespace '{}' not found", ns)
                ));
            }
        }
        
        // 3. Verify storage class is available
        let storage_check = Command::new("kubectl")
            .args(&["get", "storageclass"])
            .output()
            .await?;
        
        if !storage_check.status.success() {
            warn!("No storage class found - persistent volumes may not work");
        }
        
        // 4. Create and delete a test pod to verify full functionality
        self.verify_pod_creation().await?;
        
        info!("Cluster health verification complete");
        Ok(())
    }
    
    /// Verify that pods can be created and scheduled
    pub(crate) async fn verify_pod_creation(&self) -> Result<()> {
        debug!("Testing pod creation and scheduling...");
        
        let test_pod_name = format!("k3d-health-check-{}", chrono::Utc::now().timestamp());
        
        // Create a simple test pod
        let create_result = Command::new("kubectl")
            .args(&[
                "run", &test_pod_name,
                "--image=busybox:1.28",
                "--restart=Never",
                "--command", "--",
                "echo", "health check successful"
            ])
            .output()
            .await?;
        
        if !create_result.status.success() {
            return Err(K8sManagerError::GeneralError(
                "Failed to create test pod".to_string()
            ));
        }
        
        // Give the pod some time to be scheduled
        sleep(Duration::from_secs(2)).await;
        
        // Check pod status multiple times
        let mut attempts = 0;
        let max_attempts = 15;
        let mut pod_ready = false;
        
        while attempts < max_attempts {
            let status_output = Command::new("kubectl")
                .args(&[
                    "get", "pod", &test_pod_name,
                    "-o", "jsonpath={.status.phase}"
                ])
                .output()
                .await?;
                
            if status_output.status.success() {
                let phase = String::from_utf8_lossy(&status_output.stdout);
                debug!("Pod {} status: {}", test_pod_name, phase);
                
                if phase.trim() == "Succeeded" || phase.trim() == "Running" {
                    pod_ready = true;
                    break;
                }
            }
            
            attempts += 1;
            sleep(Duration::from_secs(2)).await;
        }
        
        // Clean up the test pod
        let _ = Command::new("kubectl")
            .args(&["delete", "pod", &test_pod_name, "--grace-period=0", "--force"])
            .status()
            .await;
        
        if pod_ready {
            debug!("Pod creation test successful");
            Ok(())
        } else {
            // Don't fail the entire setup for this - just warn
            warn!("Pod scheduling test did not complete in time - cluster may need more time to stabilize");
            Ok(())
        }
    }
    
    /// Create a new cluster
    pub async fn create_cluster(&self) -> Result<()> {
        // Use the registry name from config if it's cluster-specific
        let registry_name = if self.config.registry.name.contains(&self.config.name) {
            self.config.registry.name.clone()
        } else if self.config.registry.name == "registry.localhost" {
            format!("{}-registry", self.config.name)
        } else {
            self.config.registry.name.clone()
        };
        
        // For create_cluster, we'll rely on setup_registry being called from setup_cluster
        // This avoids duplicate registry creation
        if self.config.registry.enabled && !self.cluster_exists().await? {
            // Only create registry if we're part of a standalone create_cluster call
            // Check if this is being called from setup_cluster by checking if registry exists
            let check_output = Command::new("docker")
                .args(&["ps", "-a", "--format", "{{.Names}}"])
                .output()
                .await?;
            
            let existing_containers = String::from_utf8_lossy(&check_output.stdout);
            let full_registry_name = format!("k3d-{}", registry_name);
            
            if !existing_containers.lines().any(|name| name == full_registry_name) {
                // This is a standalone create_cluster call, so we need to create the registry
                self.setup_registry().await?;
            }
        }
        
        let mut cmd = Command::new("k3d");
        cmd.args(&[
            "cluster",
            "create",
            &self.config.name,
            "--api-port",
            &format!("{}:{}", self.config.network.api_bind_address, self.config.api_port),
            "-p",
            &format!("{}:{}@loadbalancer", self.config.http_port, self.config.http_port),
            "-p",
            &format!("{}:{}@loadbalancer", self.config.https_port, self.config.https_port),
        ]);

        // Add registry if enabled
        if self.config.registry.enabled {
            cmd.args(&[
                "--registry-use",
                &format!("k3d-{}:{}", registry_name, self.config.registry.port),
            ]);
        }

        cmd.args(&[
            "--image",
            &format!("rancher/k3s:{}", self.config.k3s_version),
            "--servers",
            &self.config.servers.to_string(),
            "--agents",
            &self.config.agents.to_string(),
        ]);

        if self.config.isolate_kubeconfig {
            cmd.args(&[
                "--kubeconfig-update-default=false",
                "--kubeconfig-switch-context=false",
            ]);
        }

        let status = cmd.status().await
            .map_err(|e| K8sManagerError::ProcessError(format!("Failed to create cluster: {}", e)))?;

        if !status.success() {
            return Err(K8sManagerError::GeneralError("Failed to create cluster".to_string()));
        }

        // Wait for cluster to be fully initialized
        info!("Waiting for cluster to initialize...");
        sleep(Duration::from_secs(3)).await;

        if self.config.isolate_kubeconfig {
            // WARNING: `fix_kubeconfig_for_container` and `use-context` both
            // write to the DEFAULT kubeconfig (no `--kubeconfig` flag of
            // their own) — running either here would defeat the isolation
            // this branch exists for. Callers that set
            // `isolate_kubeconfig` fetch a cluster-scoped kubeconfig
            // themselves via `k3d kubeconfig get`, which only prints to
            // stdout and never touches the default file.
            return Ok(());
        }

        // Fix the kubeconfig if we're in a container
        self.fix_kubeconfig_for_container().await?;

        // Set kubectl context
        let _ = Command::new("kubectl")
            .args(&[
                "config",
                "use-context",
                &format!("k3d-{}", self.config.name),
            ])
            .status()
            .await;


        Ok(())
    }

    /// Delete the cluster.
    ///
    /// Order matters: the registry container references the cluster's
    /// docker network, so it is removed first; then the cluster (which
    /// takes the network with it in the common case); then the network and
    /// the `-images` volume are removed defensively in case a still-attached
    /// container (the registry, or a leftover node) kept either alive.
    pub async fn delete_cluster(&self) -> Result<()> {
        info!("Deleting K3D cluster '{}'", self.config.name);

        if self.config.registry.enabled {
            let registry_name = if self.config.registry.name.contains(&self.config.name) {
                self.config.registry.name.clone()
            } else if self.config.registry.name == "registry.localhost" {
                format!("{}-registry", self.config.name)
            } else {
                self.config.registry.name.clone()
            };
            info!("Deleting associated registry '{}'", registry_name);

            let _ = Command::new("k3d")
                .args(&["registry", "delete", &registry_name])
                .status()
                .await;

            // Wait a bit for cleanup
            sleep(Duration::from_secs(1)).await;
        }

        let status = Command::new("k3d")
            .args(&["cluster", "delete", &self.config.name])
            .status()
            .await?;

        if !status.success() {
            return Err(K8sManagerError::GeneralError("Failed to delete cluster".to_string()));
        }

        let network_name = format!("k3d-{}", self.config.name);
        let _ = Command::new("docker")
            .args(&["network", "rm", &network_name])
            .status()
            .await;

        let images_volume = format!("k3d-{}-images", self.config.name);
        let _ = Command::new("docker")
            .args(&["volume", "rm", &images_volume])
            .status()
            .await;

        Ok(())
    }
    
    /// Check if cluster exists
    pub async fn cluster_exists(&self) -> Result<bool> {
        let output = Command::new("k3d")
            .args(&["cluster", "list"])
            .output()
            .await
            .map_err(|e| K8sManagerError::ProcessError(format!("Failed to list clusters: {}", e)))?;
        
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.contains(&self.config.name))
    }
    
    /// Get cluster information
    pub async fn get_cluster_info(&self) -> Result<ClusterInfo> {
        let mut metadata = HashMap::new();
        
        // Get cluster status
        let output = Command::new("k3d")
            .args(&["cluster", "list", "-o", "json"])
            .output()
            .await?;
        
        let mut node_count = 0;
        let mut status = ClusterStatus::Unknown;
        
        if output.status.success() {
            let clusters_json = String::from_utf8_lossy(&output.stdout);
            if let Ok(clusters) = serde_json::from_str::<serde_json::Value>(&clusters_json) {
                if let Some(cluster_array) = clusters.as_array() {
                    for cluster in cluster_array {
                        if cluster["name"] == self.config.name {
                            let servers = cluster["serversCount"].as_u64().unwrap_or(0);
                            let agents = cluster["agentsCount"].as_u64().unwrap_or(0);
                            node_count = (servers + agents) as u32;
                            
                            metadata.insert("servers".to_string(), servers.to_string());
                            metadata.insert("agents".to_string(), agents.to_string());
                            
                            // Determine status (k3d doesn't provide detailed status)
                            status = ClusterStatus::Running;
                        }
                    }
                }
            }
        }
        
        // Get endpoint based on context
        let endpoint = self.get_api_endpoint();
        
        Ok(ClusterInfo {
            name: self.config.name.clone(),
            provider: ClusterProvider::K3d,
            status,
            endpoint: Some(endpoint),
            kubernetes_version: Some(self.config.k3s_version.clone()),
            node_count,
            metadata,
        })
    }
    
    /// Get cluster status
    pub async fn get_cluster_status(&self) -> Result<String> {
        let info = self.get_cluster_info().await?;
        Ok(format!("{:?}", info.status))
    }
    
    /// Connect to existing cluster
    pub async fn connect_cluster(&self) -> Result<()> {
        self.reconnect_cluster().await
    }
    
    /// Start a stopped cluster
    pub async fn start_cluster(&self) -> Result<()> {
        let status = Command::new("k3d")
            .args(&["cluster", "start", &self.config.name])
            .status()
            .await?;
        
        if !status.success() {
            return Err(K8sManagerError::GeneralError("Failed to start cluster".to_string()));
        }
        
        Ok(())
    }
    
    /// Stop a running cluster
    pub async fn stop_cluster(&self) -> Result<()> {
        let status = Command::new("k3d")
            .args(&["cluster", "stop", &self.config.name])
            .status()
            .await?;
        
        if !status.success() {
            return Err(K8sManagerError::GeneralError("Failed to stop cluster".to_string()));
        }
        
        Ok(())
    }
    
    /// Get the appropriate API endpoint based on context
    pub(crate) fn get_api_endpoint(&self) -> String {
        get_api_endpoint(&self.config.name, self.config.api_port, &self.config.network.api_bind_address)
    }
}

// Standalone functions for backward compatibility

/// Setup K3D cluster with default configuration
pub async fn setup_cluster() -> Result<()> {
    let config = K3dConfig::default();
    setup_cluster_with_config(&config).await
}

/// Setup K3D cluster with custom configuration
pub async fn setup_cluster_with_config(config: &K3dConfig) -> Result<()> {
    // Check if k3d is installed
    check_k3d_installed().await?;
    
    // Convert basic config to full config
    let full_config = K3dClusterConfig {
        name: config.cluster_name.clone(),
        api_port: config.api_port,
        http_port: config.lb_port,
        https_port: 443,
        servers: config.servers,
        agents: config.agents,
        registry: crate::K3dRegistryConfig {
            enabled: true,
            name: config.registry_name.clone(),
            port: config.registry_port,
        },
        ..Default::default()
    };
    
    let manager = K3dClusterManager::new(full_config);
    
    // Use simplified setup for basic config
    if manager.cluster_exists().await? {
        info!("K3D cluster '{}' already exists", config.cluster_name);
        return Ok(());
    }
    
    // Create registry
    crate::providers::k3d::registry::create_registry(config).await?;
    
    // Create cluster
    create_cluster(config).await?;
    
    // Wait for cluster
    wait_for_cluster(&config.cluster_name).await?;
    
    // Configure kubectl
    configure_kubectl(&config.cluster_name).await?;
    
    info!("K3D cluster '{}' is ready", config.cluster_name);
    Ok(())
}

/// Check if k3d is installed
pub async fn check_k3d_installed() -> Result<()> {
    match which::which("k3d") {
        Ok(path) => {
            info!("k3d found at: {:?}", path);
            
            // Get version
            match Command::new("k3d")
                .arg("version")
                .output()
                .await
            {
                Ok(output) if output.status.success() => {
                    let version = String::from_utf8_lossy(&output.stdout);
                    info!("k3d version: {}", version.trim());
                    Ok(())
                }
                _ => Err(K8sManagerError::GeneralError(
                    "Failed to get k3d version".to_string()
                )),
            }
        }
        Err(_) => Err(K8sManagerError::ToolNotInstalled("k3d".to_string())),
    }
}

/// Create K3D cluster
async fn create_cluster(config: &K3dConfig) -> Result<()> {
    info!("Creating K3D cluster '{}'", config.cluster_name);
    
    let mut cmd = Command::new("k3d");
    cmd.args(&[
        "cluster",
        "create",
        &config.cluster_name,
        "--api-port",
        &config.api_port.to_string(),
        "-p",
        &format!("{}:80@loadbalancer", config.lb_port),
        "--agents",
        &config.agents.to_string(),
        "--servers",
        &config.servers.to_string(),
        "--registry-use",
        &format!("{}:{}", config.registry_name, config.registry_port),
        "--k3s-arg",
        "--disable=traefik@server:*",
        // Lower kubelet eviction thresholds so a host disk at 90% used
        // (common on dev machines) doesn't taint nodes with
        // disk-pressure:NoSchedule and block all chart pod scheduling.
        // The k3d node's filesystem view IS the host's filesystem, so the
        // default `nodefs.available<10%` taints the node any time the
        // host is over 90% full.
        "--k3s-arg",
        "--kubelet-arg=eviction-hard=nodefs.available<2%,imagefs.available<2%@agent:*",
        "--k3s-arg",
        "--kubelet-arg=eviction-hard=nodefs.available<2%,imagefs.available<2%@server:*",
        "--wait",
    ]);
    
    let status = cmd.status().await
        .map_err(|e| K8sManagerError::ProcessError(format!("Failed to create cluster: {}", e)))?;
    
    if !status.success() {
        return Err(K8sManagerError::GeneralError(
            "Failed to create K3D cluster".to_string()
        ));
    }
    
    // Additional wait to ensure cluster is fully ready
    sleep(Duration::from_secs(2)).await;
    
    Ok(())
}

/// Wait for cluster to be ready
async fn wait_for_cluster(cluster_name: &str) -> Result<()> {
    info!("Waiting for cluster '{}' to be ready...", cluster_name);
    
    // Create a temporary manager with default config for timeout values
    let config = K3dClusterConfig {
        name: cluster_name.to_string(),
        ..Default::default()
    };
    let manager = K3dClusterManager::new(config);
    
    // Use the comprehensive wait method
    manager.wait_for_cluster().await
}

/// Configure kubectl to use the cluster
async fn configure_kubectl(cluster_name: &str) -> Result<()> {
    let status = Command::new("k3d")
        .args(&["kubeconfig", "merge", cluster_name])
        .status()
        .await
        .map_err(|e| K8sManagerError::ProcessError(format!("Failed to configure kubectl: {}", e)))?;
    
    if !status.success() {
        return Err(K8sManagerError::GeneralError(
            "Failed to configure kubectl".to_string()
        ));
    }
    
    // Set as current context
    let status = Command::new("kubectl")
        .args(&["config", "use-context", &format!("k3d-{}", cluster_name)])
        .status()
        .await?;
    
    if !status.success() {
        warn!("Failed to set kubectl context");
    }
    
    Ok(())
}

/// Delete K3D cluster
pub async fn delete_cluster(cluster_name: &str) -> Result<()> {
    info!("Deleting K3D cluster '{}'", cluster_name);
    
    let status = Command::new("k3d")
        .args(&["cluster", "delete", cluster_name])
        .status()
        .await
        .map_err(|e| K8sManagerError::ProcessError(format!("Failed to delete cluster: {}", e)))?;
    
    if !status.success() {
        return Err(K8sManagerError::GeneralError(
            "Failed to delete K3D cluster".to_string()
        ));
    }
    
    Ok(())
}