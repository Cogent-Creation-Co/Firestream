//! Command execution logic
//!
//! This module implements the actual command execution for the CLI.

use super::args::{Cli, Command, ConfigCommand, StateCommand, ClusterCommand, HelmCommand};
use crate::app;
use crate::services::ServiceManager;
use crate::config::{ConfigManager, ServiceState, ServiceStatus, ResourceUsage};
use crate::state::{StateManager, ResourceType, K3dClusterConfig, FirestreamState};
use k8s_manager::K3dClusterManager;
use crate::core::{FirestreamError, Result};
use std::path::PathBuf;
use tracing::{info, error};
use helm_manager::kubectl_client::KubectlClient;

/// Execute the CLI command
pub async fn execute_command(cli: Cli) -> Result<()> {
    // Initialize services
    let service_manager = ServiceManager::new().await
        .map_err(|e| FirestreamError::GeneralError(format!("Failed to initialize service manager: {}", e)))?;
    
    let config_manager = ConfigManager::new()
        .map_err(|e| FirestreamError::GeneralError(format!("Failed to initialize config manager: {}", e)))?;

    // Get state directory
    let state_dir = PathBuf::from(".firestream/state");
    
    match cli.command {
        Some(Command::Init { name }) => {
            execute_init(name).await?;
        }
        
        Some(Command::Plan { target, output, out }) => {
            execute_plan(&state_dir, target, &output, out).await?;
        }
        
        Some(Command::Apply { plan, auto_approve, target }) => {
            execute_apply(&state_dir, plan, auto_approve, target).await?;
        }
        
        Some(Command::Refresh) => {
            execute_refresh(&state_dir).await?;
        }
        
        Some(Command::Import { resource_type, resource_id, data }) => {
            execute_import(&state_dir, &resource_type, &resource_id, data).await?;
        }
        
        Some(Command::State { ref command }) => {
            execute_state_command(command, &state_dir).await?;
        }
        
        Some(Command::Install { service, config }) => {
            let config_path = config.as_ref().map(|p| p.to_str().unwrap());
            service_manager.install_service(&service, config_path).await?;
            if !cli.quiet {
                println!("Service {} installed successfully!", service);
            }
        }
        
        Some(Command::Uninstall { service: _ }) => {
            // TODO: Implement uninstall
            error!("Uninstall command not yet implemented");
            return Err(FirestreamError::GeneralError("Not implemented".to_string()));
        }
        
        Some(Command::Start { service }) => {
            service_manager.start_service(&service).await?;
            if !cli.quiet {
                println!("Service {} started successfully!", service);
            }
        }
        
        Some(Command::Stop { service }) => {
            service_manager.stop_service(&service).await?;
            if !cli.quiet {
                println!("Service {} stopped successfully!", service);
            }
        }
        
        Some(Command::Restart { service }) => {
            service_manager.stop_service(&service).await?;
            service_manager.start_service(&service).await?;
            if !cli.quiet {
                println!("Service {} restarted successfully!", service);
            }
        }
        
        Some(Command::Status { service }) => {
            let states = service_manager.get_status(service.as_deref()).await?;
            
            if cli.json {
                let json = serde_json::to_string_pretty(&states)
                    .map_err(|e| FirestreamError::GeneralError(format!("Failed to serialize to JSON: {}", e)))?;
                println!("{}", json);
            } else {
                print_status(&states, cli.quiet);
            }
        }
        
        Some(Command::List { available }) => {
            if available {
                let services = service_manager.list_available_services().await?;
                
                if cli.json {
                    let json = serde_json::to_string_pretty(&services)
                        .map_err(|e| FirestreamError::GeneralError(format!("Failed to serialize to JSON: {}", e)))?;
                    println!("{}", json);
                } else {
                    println!("Available services:");
                    for service in services {
                        println!("  {}", service);
                    }
                }
            } else {
                let states = service_manager.get_status(None).await?;
                
                if cli.json {
                    let json = serde_json::to_string_pretty(&states)
                        .map_err(|e| FirestreamError::GeneralError(format!("Failed to serialize to JSON: {}", e)))?;
                    println!("{}", json);
                } else {
                    println!("Installed services:");
                    for (name, state) in states {
                        println!("  {} - {:?}", name, state.status);
                    }
                }
            }
        }
        
        Some(Command::Config { ref command }) => {
            execute_config_command(command, &config_manager, &cli).await?;
        }
        
        Some(Command::Logs { service, follow, lines }) => {
            let logs = service_manager.get_logs(&service, follow).await?;
            
            let start_idx = if logs.len() > lines {
                logs.len() - lines
            } else {
                0
            };
            
            for line in &logs[start_idx..] {
                println!("{}", line);
            }
            
            if follow {
                // TODO: Implement actual log following
                println!("Log following not yet implemented");
            }
        }
        
        Some(Command::Resources { service }) => {
            let usage = service_manager.get_resource_usage(service.as_deref()).await?;
            
            if cli.json {
                let json = serde_json::to_string_pretty(&usage)
                    .map_err(|e| FirestreamError::GeneralError(format!("Failed to serialize to JSON: {}", e)))?;
                println!("{}", json);
            } else {
                print_resource_usage(&usage);
            }
        }
        
        Some(Command::Tui) => {
            // Launch TUI
            crate::tui::run().await?;
        }
        
        Some(Command::Cluster { ref command }) => {
            execute_cluster_command(command, &state_dir).await?;
        }
        
        Some(Command::Build { ref packages, native, docker, parallel: _, timeout }) => {
            execute_build(packages, native, docker, timeout, cli.quiet, cli.json).await?;
        }

        Some(Command::Helm { ref command }) => {
            execute_helm_command(command, &cli.charts_dir).await?;
        }

        Some(Command::App { ref command }) => {
            app::execute_app_command(command, &cli).await?;
        }

        Some(Command::Template { name, project_type, output, non_interactive, values }) => {
            crate::template::execute_template(name, &project_type, &output, non_interactive, values.as_ref()).await?;
        }
        
        None => {
            // No command specified, launch TUI by default
            crate::tui::run().await?;
        }
    }

    Ok(())
}

/// Execute init command
async fn execute_init(name: Option<String>) -> Result<()> {
    let project_name = name.unwrap_or_else(|| "firestream-project".to_string());
    
    // Create project structure
    let project_dir = PathBuf::from(&project_name);
    tokio::fs::create_dir_all(&project_dir).await?;
    tokio::fs::create_dir_all(project_dir.join(".firestream/state")).await?;
    tokio::fs::create_dir_all(project_dir.join("infrastructure")).await?;
    tokio::fs::create_dir_all(project_dir.join("deployments")).await?;
    tokio::fs::create_dir_all(project_dir.join("builds")).await?;
    
    // Create default config
    let config = crate::config::GlobalConfig::default();
    let config_path = project_dir.join("firestream.toml");
    let config_str = toml::to_string_pretty(&config)?;
    tokio::fs::write(&config_path, config_str).await?;
    
    // Initialize state
    let state_manager = StateManager::new(project_dir.join(".firestream/state"));
    state_manager.init().await?;
    
    println!("Initialized Firestream project in '{}'", project_name);
    println!("\nNext steps:");
    println!("  1. cd {}", project_name);
    println!("  2. Edit firestream.toml to configure your project");
    println!("  3. Run 'firestream plan' to see what will be created");
    println!("  4. Run 'firestream apply' to create resources");
    
    Ok(())
}

/// Execute plan command
async fn execute_plan(state_dir: &PathBuf, targets: Vec<String>, output: &str, out: Option<PathBuf>) -> Result<()> {
    let mut state_manager = StateManager::new(state_dir.clone());
    
    // Lock state
    state_manager.lock().await?;
    
    // Load config
    let config_manager = ConfigManager::new()?;
    let config = config_manager.load_global_config().await?;
    
    // Create plan
    let plan = state_manager.plan(&config, targets).await?;
    
    // Output plan
    match output {
        "json" => {
            let json = serde_json::to_string_pretty(&plan)?;
            if let Some(out_path) = out {
                tokio::fs::write(&out_path, &json).await?;
                println!("Plan saved to {:?}", out_path);
            } else {
                println!("{}", json);
            }
        }
        _ => {
            // Text output
            println!("\nFirestream will perform the following actions:\n");
            
            for change in &plan.changes {
                let symbol = match change.change_type {
                    crate::state::ChangeType::Create => "+",
                    crate::state::ChangeType::Update => "~",
                    crate::state::ChangeType::Delete => "-",
                    crate::state::ChangeType::Replace => "!",
                    crate::state::ChangeType::NoOp => " ",
                };
                
                println!("  {} {}", symbol, change.description);
            }
            
            println!("\nPlan: {} changes", plan.changes.len());
            
            if let Some(out_path) = out {
                let json = serde_json::to_string_pretty(&plan)?;
                tokio::fs::write(&out_path, &json).await?;
                println!("\nPlan saved to {:?}", out_path);
            }
        }
    }
    
    // Unlock state
    state_manager.unlock().await?;
    
    Ok(())
}

/// Execute apply command
async fn execute_apply(state_dir: &PathBuf, plan_id: Option<String>, auto_approve: bool, targets: Vec<String>) -> Result<()> {
    let mut state_manager = StateManager::new(state_dir.clone());
    
    // Lock state
    state_manager.lock().await?;
    
    let result = if let Some(plan_id) = plan_id {
        // Apply specific plan
        state_manager.apply(&plan_id, auto_approve).await
    } else {
        // Create and apply new plan
        let config_manager = ConfigManager::new()?;
        let config = config_manager.load_global_config().await?;
        
        let plan = state_manager.plan(&config, targets).await?;
        state_manager.apply(&plan.id, auto_approve).await
    };
    
    // Unlock state
    state_manager.unlock().await?;
    
    result?;
    
    println!("\nApply complete! Resources are up-to-date.");
    Ok(())
}

/// Execute refresh command
async fn execute_refresh(state_dir: &PathBuf) -> Result<()> {
    let mut state_manager = StateManager::new(state_dir.clone());
    
    // Lock state
    state_manager.lock().await?;
    
    println!("Refreshing state...");
    state_manager.refresh().await?;
    
    // Unlock state
    state_manager.unlock().await?;
    
    println!("State refreshed successfully.");
    Ok(())
}

/// Execute import command
async fn execute_import(state_dir: &PathBuf, resource_type: &str, resource_id: &str, data_file: Option<PathBuf>) -> Result<()> {
    let mut state_manager = StateManager::new(state_dir.clone());
    
    // Parse resource type
    let rt = match resource_type {
        "infrastructure" => ResourceType::Infrastructure,
        "build" => ResourceType::Build,
        "deployment" => ResourceType::Deployment,
        "cluster" => ResourceType::Cluster,
        _ => return Err(FirestreamError::GeneralError(format!("Invalid resource type: {}", resource_type))),
    };
    
    // Load resource data
    let data = if let Some(path) = data_file {
        let content = tokio::fs::read_to_string(&path).await?;
        serde_json::from_str(&content)?
    } else {
        serde_json::json!({})
    };
    
    // Lock state
    state_manager.lock().await?;
    
    state_manager.import(rt, resource_id.to_string(), data).await?;
    
    // Unlock state
    state_manager.unlock().await?;
    
    println!("Imported {} '{}' successfully.", resource_type, resource_id);
    Ok(())
}

/// Execute state subcommands
async fn execute_state_command(command: &StateCommand, state_dir: &PathBuf) -> Result<()> {
    match command {
        StateCommand::Show { resource, output } => {
            let state_manager = StateManager::new(state_dir.clone());
            let mut sm = state_manager;
            let state = sm.load().await?;
            
            match output.as_str() {
                "json" => {
                    let json = if let Some(res) = resource {
                        // Show specific resource
                        serde_json::json!({
                            "resource": res,
                            "state": state
                        })
                    } else {
                        serde_json::to_value(state)?
                    };
                    println!("{}", serde_json::to_string_pretty(&json)?);
                }
                _ => {
                    // Text output
                    println!("State Information:");
                    println!("  Version: {}", state.version);
                    println!("  Serial: {}", state.serial);
                    println!("  Last Modified: {}", state.last_modified);
                    println!("  Last Modified By: {}", state.last_modified_by);
                    println!("\nResources:");
                    println!("  Infrastructure: {}", if state.infrastructure.stack_name.is_some() { "Configured" } else { "Not configured" });
                    println!("  Builds: {}", state.builds.len());
                    println!("  Deployments: {}", state.deployments.len());
                    println!("  Cluster: {}", if state.cluster.name.is_some() { "Configured" } else { "Not configured" });
                }
            }
        }
        
        StateCommand::Lock { timeout: _ } => {
            let mut state_manager = StateManager::new(state_dir.clone());
            state_manager.lock().await?;
            println!("State locked successfully.");
        }
        
        StateCommand::Unlock { force } => {
            if *force {
                let lock = crate::state::StateLock::force_acquire(state_dir).await?;
                lock.release().await?;
            } else {
                let mut state_manager = StateManager::new(state_dir.clone());
                state_manager.unlock().await?;
            }
            println!("State unlocked successfully.");
        }
        
        StateCommand::Locks => {
            if let Some(lock_info) = crate::state::StateLock::get_info(state_dir).await {
                println!("Active locks:");
                println!("  ID: {}", lock_info.id);
                println!("  User: {}", lock_info.user);
                println!("  Host: {}", lock_info.hostname);
                println!("  PID: {}", lock_info.pid);
                println!("  Acquired: {}", lock_info.acquired_at);
                println!("  Expires: {}", lock_info.expires_at);
            } else {
                println!("No active locks.");
            }
        }
        
        StateCommand::Pull => {
            println!("Remote state management not yet implemented.");
        }
        
        StateCommand::Push => {
            println!("Remote state management not yet implemented.");
        }
    }
    
    Ok(())
}

/// Execute config subcommands
async fn execute_config_command(command: &ConfigCommand, config_manager: &ConfigManager, cli: &Cli) -> Result<()> {
    match command {
        ConfigCommand::Show { service } => {
            let config = config_manager.load_service_config(&service).await
                .map_err(|e| FirestreamError::ConfigError(format!("Failed to load config: {}", e)))?;
            
            if cli.json {
                let json = serde_json::to_string_pretty(&config)
                    .map_err(|e| FirestreamError::GeneralError(format!("Failed to serialize to JSON: {}", e)))?;
                println!("{}", json);
            } else {
                let toml = toml::to_string_pretty(&config)
                    .map_err(|e| FirestreamError::GeneralError(format!("Failed to serialize to TOML: {}", e)))?;
                println!("{}", toml);
            }
        }
        
        ConfigCommand::Edit { service: _ } => {
            // TODO: Implement config editing
            error!("Config edit command not yet implemented");
            return Err(FirestreamError::GeneralError("Not implemented".to_string()));
        }
        
        ConfigCommand::Validate { target } => {
            // TODO: Implement config validation
            info!("Validating configuration: {}", target);
            println!("Configuration is valid");
        }
    }
    
    Ok(())
}

/// Print service status in a formatted way
fn print_status(states: &std::collections::HashMap<String, ServiceState>, quiet: bool) {
    
    if quiet {
        for (name, state) in states {
            println!("{}\t{:?}", name, state.status);
        }
    } else {
        println!("{:<20} {:<15} {:<10} {:<15} {:<10}",
                 "Service", "Status", "Version", "Last Updated", "Resources");
        println!("{}", "-".repeat(80));
        
        for (name, state) in states {
            let version = state.installed_version.as_deref().unwrap_or("-");
            let updated = state.last_updated.format("%Y-%m-%d %H:%M");
            let resources = if let Some(usage) = &state.resource_usage {
                format!("{:.1}CPU {:.0}MB", usage.cpu_cores, usage.memory_mb)
            } else {
                "-".to_string()
            };
            
            let status_str = match &state.status {
                ServiceStatus::Running => "Running",
                ServiceStatus::Stopped => "Stopped",
                ServiceStatus::Installing => "Installing",
                ServiceStatus::NotInstalled => "Not Installed",
                ServiceStatus::Error(_) => "Error",
            };
            
            println!("{:<20} {:<15} {:<10} {:<15} {:<10}",
                     name, status_str, version, updated, resources);
        }
    }
}

/// Print resource usage in a formatted way
fn print_resource_usage(usage: &std::collections::HashMap<String, ResourceUsage>) {
    println!("{:<20} {:<10} {:<15} {:<10}",
             "Service", "CPU", "Memory", "Disk");
    println!("{}", "-".repeat(60));
    
    for (name, resources) in usage {
        println!("{:<20} {:<10.2} {:<15} {:<10}",
                 name,
                 resources.cpu_cores,
                 format!("{}MB", resources.memory_mb),
                 format!("{}GB", resources.disk_gb));
    }
}

/// Execute cluster subcommands
async fn execute_cluster_command(command: &ClusterCommand, state_dir: &PathBuf) -> Result<()> {
    match command {
        ClusterCommand::Create { name, config: config_file, clean, servers, agents, dev_mode } => {
            let config = if let Some(config_path) = config_file {
                // Load config from file
                let content = tokio::fs::read_to_string(config_path).await?;
                let parsed: toml::Value = toml::from_str(&content)?;
                
                // Extract k3d config from TOML
                if let Some(cluster_section) = parsed.get("cluster") {
                    if let Some(k3d_section) = cluster_section.get("k3d") {
                        serde_json::from_str(&serde_json::to_string(k3d_section)?)?
                    } else {
                        return Err(FirestreamError::ConfigError("Missing [cluster.k3d] section in config".to_string()));
                    }
                } else {
                    return Err(FirestreamError::ConfigError("Missing [cluster] section in config".to_string()));
                }
            } else {
                // Create config from CLI args
                let mut config = K3dClusterConfig::default();
                
                if let Some(cluster_name) = name {
                    config.name = cluster_name.clone();
                }
                
                config.servers = *servers;
                config.agents = *agents;
                
                if *dev_mode {
                    config.dev_mode = Some(crate::state::schema::K3dDevModeConfig {
                        port_forward_all: true,
                        port_offset: 10000,
                    });
                }
                
                config
            };
            
            let manager = K3dClusterManager::new(config.clone());
            
            if *clean {
                // Delete existing cluster first
                let _ = manager.delete_cluster().await;
            }
            
            println!("Creating K3D cluster '{}'...", config.name);
            manager.setup_cluster().await?;
            
            // Save cluster configuration to state
            let state_manager = StateManager::new(state_dir.clone());
            
            // Initialize state directory if needed
            state_manager.init().await?;
            
            // Use a manual approach to update state
            let state_file = state_dir.join("firestream.state.json");
            
            // Load existing state or create new one
            let mut state = if state_file.exists() {
                let content = tokio::fs::read_to_string(&state_file).await?;
                serde_json::from_str::<FirestreamState>(&content)?
            } else {
                FirestreamState::new()
            };
            
            // Update cluster configuration
            state.cluster.name = Some(config.name.clone());
            state.cluster.cluster_type = Some("k3d".to_string());
            state.cluster.managed = true;
            state.cluster.k3d_config = Some(config.clone());
            state.increment_serial();
            
            // Save state
            let content = serde_json::to_string_pretty(&state)?;
            tokio::fs::write(&state_file, content).await?;
            
            println!("\nCluster '{}' created successfully!", config.name);
            println!("\nKubectl context set to: k3d-{}", config.name);
            
            if config.dev_mode.is_some() {
                println!("\nDevelopment mode enabled. Services will be accessible at localhost with +10000 port offset.");
            }
        }
        
        ClusterCommand::Delete { name } => {
            let cluster_name = if let Some(n) = name {
                n.clone()
            } else {
                // Try to load from state
                let mut state_manager = StateManager::new(state_dir.clone());
                let state = state_manager.load().await?;
                
                if let Some(k3d_config) = &state.cluster.k3d_config {
                    k3d_config.name.clone()
                } else if let Some(n) = &state.cluster.name {
                    n.clone()
                } else {
                    return Err(FirestreamError::GeneralError("No cluster name specified and none found in state".to_string()));
                }
            };
            
            println!("Deleting K3D cluster '{}'...", cluster_name);
            
            let config = K3dClusterConfig {
                name: cluster_name.clone(),
                ..Default::default()
            };
            
            let manager = K3dClusterManager::new(config);
            manager.delete_cluster().await?;
            
            // Remove cluster configuration from state
            let state_file = state_dir.join("firestream.state.json");
            if state_file.exists() {
                let content = tokio::fs::read_to_string(&state_file).await?;
                let mut state = serde_json::from_str::<FirestreamState>(&content)?;
                
                // Clear cluster configuration
                state.cluster.name = None;
                state.cluster.cluster_type = None;
                state.cluster.managed = false;
                state.cluster.k3d_config = None;
                state.increment_serial();
                
                // Save updated state
                let content = serde_json::to_string_pretty(&state)?;
                tokio::fs::write(&state_file, content).await?;
            }
            
            println!("Cluster '{}' deleted successfully.", cluster_name);
        }
        
        ClusterCommand::Info { name } => {
            let cluster_name = if let Some(n) = name {
                n.clone()
            } else {
                // Try to load from state
                let mut state_manager = StateManager::new(state_dir.clone());
                let state = state_manager.load().await?;
                
                if let Some(k3d_config) = &state.cluster.k3d_config {
                    k3d_config.name.clone()
                } else if let Some(n) = &state.cluster.name {
                    n.clone()
                } else {
                    return Err(FirestreamError::GeneralError("No cluster name specified and none found in state".to_string()));
                }
            };
            
            let config = K3dClusterConfig {
                name: cluster_name.clone(),
                ..Default::default()
            };
            
            let manager = K3dClusterManager::new(config);
            let info = manager.get_cluster_info().await?;

            println!("Cluster Information:");
            println!("  Name: {}", info.name);
            println!("  Provider: {:?}", info.provider);
            println!("  Status: {:?}", info.status);
            if let Some(endpoint) = &info.endpoint {
                println!("  Endpoint: {}", endpoint);
            }
            if let Some(version) = &info.kubernetes_version {
                println!("  Kubernetes Version: {}", version);
            }
            println!("  Node Count: {}", info.node_count);

            if !info.metadata.is_empty() {
                println!("\nMetadata:");
                for (key, value) in &info.metadata {
                    println!("  {}: {}", key, value);
                }
            }
        }
        
        ClusterCommand::PortForward { service, offset } => {
            println!("Setting up port forwarding...");
            
            if service == "all" {
                // Setup port forwarding for all services
                let config = K3dClusterConfig::default();
                let manager = K3dClusterManager::new(config);
                
                let dev_config = crate::state::schema::K3dDevModeConfig {
                    port_forward_all: true,
                    port_offset: *offset,
                };
                
                manager.setup_port_forwarding(&dev_config).await?;
                
                println!("Port forwarding setup complete for all services.");
            } else {
                // Setup port forwarding for specific service
                println!("Port forwarding for specific services not yet implemented.");
            }
        }
        
        ClusterCommand::Logs { resource_type, resource_name, namespace, follow, tail, all_containers, previous } => {
            execute_cluster_logs(resource_type, resource_name.as_deref(), namespace, *follow, *tail, *all_containers, *previous).await?;
        }
        
        ClusterCommand::Diagnostics { all, nodes, pods, services, events, namespace } => {
            execute_cluster_diagnostics(*all, *nodes, *pods, *services, *events, namespace).await?;
        }
    }
    
    Ok(())
}

/// Execute cluster logs command
async fn execute_cluster_logs(
    resource_type: &str,
    resource_name: Option<&str>,
    namespace: &str,
    follow: bool,
    tail: u32,
    all_containers: bool,
    previous: bool,
) -> Result<()> {
    use tokio::process::Command;
    
    println!("Fetching logs from {} resources in namespace '{}'...", resource_type, namespace);
    
    if let Some(name) = resource_name {
        // Get logs for specific resource
        let mut cmd = Command::new("kubectl");
        cmd.arg("logs")
            .arg("-n")
            .arg(namespace)
            .arg(&format!("{}/{}", resource_type, name))
            .arg("--tail")
            .arg(tail.to_string());
        
        if follow {
            cmd.arg("-f");
        }
        
        if all_containers {
            cmd.arg("--all-containers");
        }
        
        if previous {
            cmd.arg("--previous");
        }
        
        let status = cmd.status().await?;
        
        if !status.success() {
            return Err(FirestreamError::GeneralError("Failed to get logs".to_string()));
        }
    } else {
        // Get all resources of the type
        let output = Command::new("kubectl")
            .args(&["get", resource_type, "-n", namespace, "-o", "name"])
            .output()
            .await?;
        
        if !output.status.success() {
            return Err(FirestreamError::GeneralError(format!("Failed to list {}", resource_type)));
        }
        
        let resources = String::from_utf8_lossy(&output.stdout);
        let resource_list: Vec<&str> = resources.lines().collect();
        
        if resource_list.is_empty() {
            println!("No {} found in namespace '{}'", resource_type, namespace);
            return Ok(());
        }
        
        println!("\nShowing last {} lines from each resource:\n", tail);
        
        for resource in resource_list {
            println!("\n=== {} ===", resource);
            
            let mut cmd = Command::new("kubectl");
            cmd.arg("logs")
                .arg("-n")
                .arg(namespace)
                .arg(resource)
                .arg("--tail")
                .arg(tail.to_string());
            
            if all_containers {
                cmd.arg("--all-containers");
            }
            
            let _ = cmd.status().await;
        }
    }
    
    Ok(())
}

/// Execute build command - builds container images via Nix
async fn execute_build(
    packages: &[String],
    native: bool,
    docker: bool,
    timeout: u64,
    quiet: bool,
    json: bool,
) -> Result<()> {
    use nix_container_builder::{BuildConfig, NixContainerBuilder, BuildProgress, BuildPhase};
    use std::time::Duration;

    let config = BuildConfig::default()
        .with_force_native(native)
        .with_force_docker(docker)
        .with_timeout(Duration::from_secs(timeout));

    let builder = NixContainerBuilder::with_config(config).await
        .map_err(|e| FirestreamError::BuildError(format!("Failed to initialize builder: {}", e)))?;

    let platform = builder.platform();
    let strategy = builder.recommended_strategy();

    if !quiet && !json {
        eprintln!("Platform: {} {} | Strategy: {}", platform.platform, platform.arch, strategy);
        eprintln!("Building {} package(s)...\n", packages.len());
    }

    let mut results = Vec::new();
    let mut failures = Vec::new();

    for (i, package) in packages.iter().enumerate() {
        if !quiet && !json {
            eprintln!("[{}/{}] Building .#{}...", i + 1, packages.len(), package);
        }

        let result = if quiet || json {
            builder.build_package(package).await
        } else {
            builder.build_package_with_progress(package, |progress: BuildProgress| {
                match &progress.phase {
                    BuildPhase::Resolving => eprintln!("  Resolving flake inputs..."),
                    BuildPhase::Building { .. } => eprintln!("  Building (this may take a while)..."),
                    BuildPhase::Loading => eprintln!("  Loading image into Docker..."),
                    BuildPhase::Complete => eprintln!("  Done: {}", progress.message),
                    BuildPhase::Failed => eprintln!("  FAILED: {}", progress.message),
                    _ => {}
                }
            }).await
        };

        match result {
            Ok(build_result) => {
                if !quiet && !json {
                    eprintln!("  ✓ {} -> {} ({:.1}s)\n",
                        package, build_result.full_ref(), build_result.duration.as_secs_f64());
                }
                results.push(build_result);
            }
            Err(e) => {
                if !quiet && !json {
                    eprintln!("  ✗ {} failed: {}\n", package, e);
                }
                failures.push((package.clone(), e.to_string()));
            }
        }
    }

    // Output
    if json {
        let output = serde_json::json!({
            "succeeded": results.iter().map(|r| {
                serde_json::json!({
                    "package": r.container,
                    "image": r.full_ref(),
                    "duration_secs": r.duration.as_secs_f64(),
                    "strategy": r.strategy.to_string(),
                })
            }).collect::<Vec<_>>(),
            "failed": failures.iter().map(|(pkg, err)| {
                serde_json::json!({ "package": pkg, "error": err })
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else if !quiet {
        eprintln!("Build complete: {} succeeded, {} failed", results.len(), failures.len());
    }

    if !failures.is_empty() {
        return Err(FirestreamError::BuildError(format!(
            "{} build(s) failed: {}",
            failures.len(),
            failures.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>().join(", ")
        )));
    }

    Ok(())
}

/// Execute cluster diagnostics command
async fn execute_cluster_diagnostics(
    all: bool,
    nodes: bool,
    pods: bool,
    services: bool,
    events: bool,
    namespace: &str,
) -> Result<()> {
    use tokio::process::Command;
    
    let show_nodes = all || nodes;
    let show_pods = all || pods;
    let show_services = all || services;
    let show_events = all || events;
    
    println!("Gathering cluster diagnostics...\n");
    
    // Node information
    if show_nodes {
        println!("=== NODES ===");
        let output = Command::new("kubectl")
            .args(&["get", "nodes", "-o", "wide"])
            .output()
            .await?;
        
        if output.status.success() {
            println!("{}", String::from_utf8_lossy(&output.stdout));
        }
        
        // Node resource usage
        println!("=== NODE RESOURCES ===");
        let output = Command::new("kubectl")
            .args(&["top", "nodes"])
            .output()
            .await?;
        
        if output.status.success() {
            println!("{}", String::from_utf8_lossy(&output.stdout));
        }
    }
    
    // Pod information
    if show_pods {
        println!("\n=== PODS ===");
        let mut cmd = Command::new("kubectl");
        cmd.args(&["get", "pods", "-o", "wide"]);
        
        if namespace == "all" {
            cmd.arg("--all-namespaces");
        } else {
            cmd.arg("-n").arg(namespace);
        }
        
        let output = cmd.output().await?;
        
        if output.status.success() {
            println!("{}", String::from_utf8_lossy(&output.stdout));
        }
        
        // Pod resource usage
        println!("\n=== POD RESOURCES ===");
        let mut cmd = Command::new("kubectl");
        cmd.args(&["top", "pods"]);
        
        if namespace == "all" {
            cmd.arg("--all-namespaces");
        } else {
            cmd.arg("-n").arg(namespace);
        }
        
        let output = cmd.output().await?;
        
        if output.status.success() {
            println!("{}", String::from_utf8_lossy(&output.stdout));
        }
    }
    
    // Service information
    if show_services {
        println!("\n=== SERVICES ===");
        let mut cmd = Command::new("kubectl");
        cmd.args(&["get", "services", "-o", "wide"]);
        
        if namespace == "all" {
            cmd.arg("--all-namespaces");
        } else {
            cmd.arg("-n").arg(namespace);
        }
        
        let output = cmd.output().await?;
        
        if output.status.success() {
            println!("{}", String::from_utf8_lossy(&output.stdout));
        }
    }
    
    // Events
    if show_events {
        println!("\n=== RECENT EVENTS ===");
        let mut cmd = Command::new("kubectl");
        cmd.args(&["get", "events", "--sort-by=.metadata.creationTimestamp"]);
        
        if namespace == "all" {
            cmd.arg("--all-namespaces");
        } else {
            cmd.arg("-n").arg(namespace);
        }
        
        let output = cmd.output().await?;
        
        if output.status.success() {
            let events = String::from_utf8_lossy(&output.stdout);
            // Show only last 20 events
            let lines: Vec<&str> = events.lines().collect();
            if lines.len() > 20 {
                println!("{}", lines[0]); // Header
                for line in lines.iter().skip(lines.len() - 20) {
                    println!("{}", line);
                }
            } else {
                println!("{}", events);
            }
        }
    }
    
    // Cluster health summary
    println!("\n=== CLUSTER HEALTH SUMMARY ===");
    let output = Command::new("kubectl")
        .args(&["cluster-info"])
        .output()
        .await?;

    if output.status.success() {
        println!("{}", String::from_utf8_lossy(&output.stdout));
    }

    Ok(())
}

/// Execute `firestream helm <subcommand>`.
///
/// Wires the Nix-emitted chart bundle (`firestream-charts-bundle`) through
/// the existing helm lifecycle executor. The bundle is opened once via
/// `Charts::open(charts_dir)`; per-chart manifests are parsed lazily.
async fn execute_helm_command(command: &HelmCommand, charts_dir: &PathBuf) -> Result<()> {
    use crate::deploy::helm_lifecycle::{chart_info_from_manifest, CommonChart};
    use crate::deploy::helm::deploy_chart_lifecycle;
    use firestream_charts::Charts;
    use helm_manager::kubectl_client::KubectlClient;

    let charts = Charts::open(charts_dir).map_err(|e| {
        FirestreamError::ConfigError(format!(
            "Failed to open chart bundle at {:?}: {}. Set FIRESTREAM_CHARTS_DIR or run `nix build .#firestream-charts-bundle --out-link <dir>` and pass --charts-dir.",
            charts_dir, e
        ))
    })?;

    match command {
        HelmCommand::Deploy { chart, namespace, dry_run } => {
            let manifest = charts.get(chart).map_err(|e| {
                FirestreamError::ConfigError(format!(
                    "Chart `{}` not found in {:?}: {}",
                    chart, charts_dir, e
                ))
            })?;

            let mut info = chart_info_from_manifest(&manifest);
            if let Some(ns) = namespace {
                info.custom_namespace = Some(ns.clone());
            }
            info.dry_run = *dry_run;

            info!(
                "Deploying chart `{}` (version {}) from {:?}",
                info.name, manifest.version, manifest.bundle.chart_path
            );

            let common = CommonChart::new(info);
            deploy_chart_lifecycle(Box::new(common), None).await?;
        }
        HelmCommand::DeployStack { stack, namespace, dry_run } => {
            let manifests = charts.stack(stack).map_err(|e| {
                FirestreamError::ConfigError(format!(
                    "Stack `{}` not found in {:?}: {}",
                    stack, charts_dir, e
                ))
            })?;

            if manifests.is_empty() {
                info!("Stack `{}` resolved to zero registered charts; nothing to deploy.", stack);
                return Ok(());
            }

            info!("Deploying stack `{}`: {} chart(s)", stack, manifests.len());
            for manifest in manifests {
                let mut info = chart_info_from_manifest(&manifest);
                if let Some(ns) = namespace {
                    info.custom_namespace = Some(ns.clone());
                }
                info.dry_run = *dry_run;

                info!(
                    "  -> {} (version {}) from {:?}",
                    info.name, manifest.version, manifest.bundle.chart_path
                );

                let common = CommonChart::new(info);
                deploy_chart_lifecycle(Box::new(common), None).await?;
            }
        }
        HelmCommand::Backup { chart, namespace } => {
            let manifest = charts.get(chart).map_err(|e| {
                FirestreamError::ConfigError(format!(
                    "Chart `{}` not found in {:?}: {}",
                    chart, charts_dir, e
                ))
            })?;

            let target = BackupTarget::from_manifest(&manifest, namespace.clone())?;
            let ns = target.namespace.clone();
            let cronjob = target.cronjob.clone();
            let job_name = k8s_name_trunc(format!("{}-manual-{}", cronjob, short_id()));

            let kubectl = KubectlClient::new()
                .map_err(|e| FirestreamError::KubernetesError(e.to_string()))?;

            info!(
                "Creating manual backup job `{}` from cronjob `{}` in namespace `{}`",
                job_name, cronjob, ns
            );
            kubectl
                .create_job_from_cronjob(&ns, &cronjob, &job_name)
                .await
                .map_err(|e| {
                    FirestreamError::KubernetesError(format!(
                        "Could not start backup job from cronjob `{}` (is `backup.enabled` set on chart `{}`?): {}",
                        cronjob, chart, e
                    ))
                })?;

            let timeout = BACKUP_RESTORE_TIMEOUT_SECS;
            kubectl
                .wait_for_job(&ns, &job_name, timeout)
                .await
                .map_err(|e| FirestreamError::KubernetesError(e.to_string()))?;

            info!("Backup job `{}` completed.", job_name);

            // Best-effort: surface the exact object key from the job logs.
            let key = match kubectl.get_logs(&ns, &format!("job/{}", job_name)).await {
                Ok(logs) => logs
                    .lines()
                    .rev()
                    .find_map(|l| l.split("backup complete:").nth(1).map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty()),
                Err(_) => None,
            };

            match key {
                Some(k) => println!("Backup complete. Object: {}", k),
                None => {
                    println!("Backup complete (job `{}`).", job_name);
                    println!("Discover the exact key from the job logs:");
                    println!("  kubectl logs -n {} job/{}", ns, job_name);
                    println!(
                        "or list:  aws s3 ls s3://firestream/ --recursive --endpoint-url http://seaweedfs-all-in-one.seaweedfs.svc.cluster.local:8333"
                    );
                }
            }
        }
        HelmCommand::Restore { chart, from, namespace } => {
            let manifest = charts.get(chart).map_err(|e| {
                FirestreamError::ConfigError(format!(
                    "Chart `{}` not found in {:?}: {}",
                    chart, charts_dir, e
                ))
            })?;

            let target = BackupTarget::from_manifest(&manifest, namespace.clone())?;

            let kubectl = KubectlClient::new()
                .map_err(|e| FirestreamError::KubernetesError(e.to_string()))?;

            let job_name = restore_from_backup(
                &kubectl,
                &target,
                from,
                &restore_command(&manifest.name),
                BACKUP_RESTORE_TIMEOUT_SECS,
            )
            .await?;

            println!("Restore complete from key `{}` (job `{}`).", from, job_name);
        }
    }

    Ok(())
}

/// Default deadline (seconds) for a manual backup or restore Job to finish.
const BACKUP_RESTORE_TIMEOUT_SECS: u64 = 600;

/// Everything the backup/restore arms need to find and drive one release's
/// backup CronJob, resolved from the chart manifest's `backup` contract.
#[derive(Debug, Clone)]
pub struct BackupTarget {
    /// Firestream chart name (`manifest.name`).
    pub name: String,
    /// Helm chart name, which Bitnami also uses as `app.kubernetes.io/name`.
    pub chart: String,
    pub release: String,
    pub namespace: String,
    /// Bitnami `common.names.fullname` for the release.
    pub fullname: String,
    /// `<fullname>-<cronJobSuffix>`.
    pub cronjob: String,
    /// Scale the `<fullname>` Deployment to zero around a restore.
    pub quiesce_deployment: bool,
}

impl BackupTarget {
    /// Resolve names from the manifest. `namespace` overrides the manifest's
    /// release namespace; the last fallback is `default`.
    pub fn from_manifest(
        manifest: &firestream_charts::ChartManifest,
        namespace: Option<String>,
    ) -> Result<Self> {
        let backup = manifest.backup.as_ref().ok_or_else(|| {
            FirestreamError::ConfigError(format!(
                "chart `{}` declares no backup CronJob (`_meta.backup` is unset in its chart-manifest.json)",
                manifest.name
            ))
        })?;
        let release = manifest
            .release
            .release_name
            .clone()
            .unwrap_or_else(|| manifest.name.clone());
        let namespace = namespace
            .or_else(|| manifest.release.namespace.clone())
            .unwrap_or_else(|| "default".to_string());
        let fullname = bitnami_fullname(&release, &manifest.chart);
        let cronjob = format!("{}-{}", fullname, backup.cron_job_suffix);
        Ok(Self {
            name: manifest.name.clone(),
            chart: manifest.chart.clone(),
            release,
            namespace,
            fullname,
            cronjob,
            quiesce_deployment: backup.quiesce_deployment,
        })
    }

    /// Label selector for the release's app pods. Bitnami sets
    /// `app.kubernetes.io/name` to the chart name, which excludes bundled
    /// subchart pods (e.g. odoo's postgresql) and the backup Job pods.
    pub fn app_pod_selector(&self) -> String {
        format!(
            "app.kubernetes.io/name={},app.kubernetes.io/instance={}",
            self.chart, self.release
        )
    }
}

/// The restore container's command for a Firestream chart.
///
/// PostgreSQL restores straight into the running primary with
/// [`PG_RESTORE_SCRIPT`]. Charts whose image carries `libhelpers<name>.sh`
/// (odoo) delegate to the helper in the image so the CLI holds no copy of
/// the archive format.
pub fn restore_command(chart_name: &str) -> Vec<String> {
    let script = match chart_name {
        "odoo" => ODOO_RESTORE_SCRIPT,
        _ => PG_RESTORE_SCRIPT,
    };
    vec!["bash".to_string(), "-c".to_string(), script.to_string()]
}

/// Run a restore end to end: render the one-shot Job from the deployed backup
/// CronJob, quiesce the app Deployment when the chart asks for it, apply and
/// wait for the Job, and always scale the Deployment back afterwards.
///
/// Returns the Job name. Shared by the `firestream helm restore` CLI arm and
/// the k8s e2e harness so the two cannot drift.
pub async fn restore_from_backup(
    kubectl: &KubectlClient,
    target: &BackupTarget,
    from: &str,
    command: &[String],
    timeout: u64,
) -> Result<String> {
    let ns = &target.namespace;

    // Single source of truth: inherit the deployed backup CronJob's container
    // spec (image, env incl. S3/AWS creds + DB conn + the SeaweedFS-vs-cloud-S3
    // toggles, volumes, securityContext). This works for SeaweedFS AND real
    // cloud S3 automatically, with zero duplication of the env contract.
    let cronjob_json = kubectl
        .get_resource_json("cronjob", &target.cronjob, ns)
        .await
        .map_err(|e| {
            FirestreamError::ConfigError(format!(
                "backup not configured for `{}`; restore needs the `{}` CronJob (set backup.enabled=true). Underlying error: {}",
                target.name, target.cronjob, e
            ))
        })?;
    let job_name = k8s_name_trunc(format!("{}-restore-{}", target.fullname, short_id()));
    let job_manifest = build_restore_job_json(&cronjob_json, &job_name, ns, from, command)?;

    let previous_replicas = if target.quiesce_deployment {
        Some(quiesce_deployment(kubectl, target, timeout).await?)
    } else {
        None
    };

    info!(
        "Applying restore job `{}` (inherited from cronjob `{}`) in namespace `{}` from key `{}`",
        job_name, target.cronjob, ns, from
    );
    let outcome = async {
        kubectl
            .apply_yaml(&job_manifest, Some(ns))
            .await
            .map_err(|e| FirestreamError::KubernetesError(e.to_string()))?;
        kubectl
            .wait_for_job(ns, &job_name, timeout)
            .await
            .map_err(|e| FirestreamError::KubernetesError(e.to_string()))
    }
    .await;

    if let Some(replicas) = previous_replicas {
        let resumed = resume_deployment(kubectl, target, replicas, timeout).await;
        match (&outcome, resumed) {
            (Ok(()), Err(e)) => return Err(e),
            (Err(_), Err(e)) => error!("{}", e),
            _ => {}
        }
    }

    outcome.map(|_| job_name)
}

/// Scale the release's Deployment to zero and wait until its pods are gone.
/// Returns the replica count to restore afterwards.
async fn quiesce_deployment(
    kubectl: &KubectlClient,
    target: &BackupTarget,
    timeout: u64,
) -> Result<u32> {
    let deployment = &target.fullname;
    let ns = &target.namespace;
    let replicas = kubectl
        .get_jsonpath("deployment", deployment, ns, "{.spec.replicas}")
        .await
        .map_err(|e| FirestreamError::KubernetesError(e.to_string()))?
        .parse::<u32>()
        .unwrap_or(1);

    info!(
        "Stopping deployment `{}` in namespace `{}` ({} -> 0 replicas) for the restore",
        deployment, ns, replicas
    );
    kubectl
        .scale("deployment", deployment, ns, 0)
        .await
        .map_err(|e| FirestreamError::KubernetesError(e.to_string()))?;
    kubectl
        .wait_for_pods_deleted(ns, &target.app_pod_selector(), timeout)
        .await
        .map_err(|e| FirestreamError::KubernetesError(e.to_string()))?;
    Ok(replicas)
}

/// Scale the release's Deployment back and wait for the rollout.
async fn resume_deployment(
    kubectl: &KubectlClient,
    target: &BackupTarget,
    replicas: u32,
    timeout: u64,
) -> Result<()> {
    let deployment = &target.fullname;
    let ns = &target.namespace;
    info!(
        "Starting deployment `{}` in namespace `{}` again ({} replicas)",
        deployment, ns, replicas
    );
    kubectl
        .scale("deployment", deployment, ns, replicas)
        .await
        .map_err(|e| {
            FirestreamError::KubernetesError(format!(
                "restore finished but deployment `{}` could not be scaled back to {}: {}",
                deployment, replicas, e
            ))
        })?;
    kubectl
        .rollout_status("deployment", deployment, ns, timeout)
        .await
        .map_err(|e| {
            FirestreamError::KubernetesError(format!(
                "restore finished but deployment `{}` did not become ready: {}",
                deployment, e
            ))
        })
}

/// Restore command for the odoo chart. The archive format lives with the
/// image (`src/containers/firestream/odoo/scripts/helpers.sh`); this only
/// names the entry point. `S3_BACKUP_KEY` arrives from the Job env.
const ODOO_RESTORE_SCRIPT: &str = "set -uo pipefail\nsource /opt/firestream/scripts/libhelpersodoo.sh\nodoo_restore_s3\n";

/// A short, collision-resistant suffix for ad-hoc Job names.
///
/// Uses the same `uuid` crate the rest of the codebase relies on for unique
/// identifiers (see `state/manager.rs`, `deploy/environment.rs`); no wall-clock
/// time is involved.
fn short_id() -> String {
    uuid::Uuid::new_v4().to_string()[..8].to_string()
}

/// Trim a generated name to the 63-char Kubernetes name limit, dropping any
/// trailing `-` left behind by truncation.
fn k8s_name_trunc(name: String) -> String {
    let mut s: String = name.chars().take(63).collect();
    while s.ends_with('-') {
        s.pop();
    }
    s
}

/// Replicate Bitnami's `common.names.fullname` for the common no-override case.
///
/// If the release name already contains the chart name, the chart name is not
/// appended (e.g. release `postgresql` -> `postgresql`); otherwise the result
/// is `<release>-<chart>` (e.g. release `testpg` -> `testpg-postgresql`). The
/// value is truncated to 63 chars. For a standalone postgresql release this is
/// also `postgresql.v1.primary.fullname`, i.e. the StatefulSet / primary
/// Service / Secret name and the `<...>-pgdumpall` CronJob prefix.
fn bitnami_fullname(release_name: &str, chart_name: &str) -> String {
    let base = if release_name.contains(chart_name) {
        release_name.to_string()
    } else {
        format!("{}-{}", release_name, chart_name)
    };
    k8s_name_trunc(base)
}

/// The restore container's shell script.
///
/// Mirrors the backup CronJob's conditional endpoint + addressing-style logic
/// (so it works against SeaweedFS and real cloud S3 alike) and the same
/// `PGPASSWORD`/`HOME` handling. Every connection/credential value
/// (`PGHOST`/`PGPORT`/`PGUSER`/`PGPASSWORD_FILE` and the `S3_*`/`AWS_*` env) is
/// inherited from the CronJob container; this script only adds `S3_BACKUP_KEY`
/// resolution. `--from` may be a bucket-relative key OR a full `s3://…` URI.
// NB: no `set -e` — the retry loop owns exit handling. Object stores (and
// fresh-cluster DNS) can transiently refuse connections; a single blip should
// not fail the restore. We retry the download→psql pipeline a bounded number of
// times. The dump is produced by `pg_dumpall --clean --if-exists`, so
// re-applying after a partial failure is idempotent (DROP ... IF EXISTS then
// recreate). Tunable via FIRESTREAM_RESTORE_MAX_ATTEMPTS.
const PG_RESTORE_SCRIPT: &str = r#"set -uo pipefail
export PGPASSWORD="${PGPASSWORD:-$(cat "$PGPASSWORD_FILE" 2>/dev/null || true)}"
export HOME="${HOME:-/tmp}"
if [ -n "${S3_ADDRESSING_STYLE:-}" ]; then aws configure set default.s3.addressing_style "$S3_ADDRESSING_STYLE"; fi
uri="$S3_BACKUP_KEY"
case "$uri" in s3://*) ;; *) uri="s3://$S3_BACKUP_BUCKET/$uri" ;; esac
# Preflight: a freshly-scheduled Job pod can take a while before it can reach a
# cross-namespace ClusterIP (kube-proxy/DNS settling → botocore "Could not
# connect"). Poll a cheap `s3 ls` until reachable before pulling the dump.
waitn="${FIRESTREAM_RESTORE_S3_WAIT_ATTEMPTS:-60}"
waiti="${FIRESTREAM_RESTORE_S3_WAIT_INTERVAL:-5}"
ready=""
i=1
while [ "$i" -le "$waitn" ]; do
  if aws s3 ls "s3://$S3_BACKUP_BUCKET/" ${S3_ENDPOINT_URL:+--endpoint-url "$S3_ENDPOINT_URL"} >/dev/null 2>&1; then
    ready=1; break
  fi
  echo "[firestream] waiting for S3 endpoint ($i/$waitn)..." >&2
  i=$((i + 1)); sleep "$waiti"
done
if [ -z "$ready" ]; then echo "[firestream] S3 endpoint unreachable after $waitn attempts" >&2; exit 1; fi
echo "[firestream] restoring from $uri"
max="${FIRESTREAM_RESTORE_MAX_ATTEMPTS:-3}"
attempt=1
while :; do
  if aws s3 cp "$uri" - ${S3_ENDPOINT_URL:+--endpoint-url "$S3_ENDPOINT_URL"} | gunzip -c | psql -h "$PGHOST" -p "$PGPORT" -U "$PGUSER" -d postgres; then
    echo "[firestream] restore complete"
    exit 0
  fi
  if [ "$attempt" -ge "$max" ]; then
    echo "[firestream] restore failed after $max attempts" >&2
    exit 1
  fi
  echo "[firestream] restore attempt $attempt failed; retrying in 10s..." >&2
  attempt=$((attempt + 1))
  sleep 10
done
"#;

/// Build a one-shot restore Job by INHERITING the deployed backup CronJob's
/// container spec.
///
/// `cronjob` is the full `kubectl get cronjob … -o json` object. We lift
/// `spec.jobTemplate.spec.template.spec` (the pod spec) verbatim — image, env
/// (including S3/AWS creds whether literal or `secretKeyRef`, the
/// SeaweedFS-vs-cloud toggles, and the PG connection vars), volumes,
/// volumeMounts, securityContext, imagePullSecrets, nodeSelector, tolerations —
/// and only:
///   * replace the container `command` with `command` (see [`restore_command`]),
///   * append `S3_BACKUP_KEY=<from>`,
///   * drop the backup-only `PGDUMP_DIR` env var,
///   * force `restartPolicy: Never`.
///
/// This is the single source of truth shared by the `firestream helm restore`
/// CLI arm and the Phase-3 e2e harness, so the two can never drift.
///
/// `cronjob_json` is the raw `kubectl get cronjob … -o json` output (parsed
/// here, so callers don't need their own `serde_json` dependency). The output
/// is a JSON string (`kubectl apply -f -` accepts JSON).
pub fn build_restore_job_json(
    cronjob_json: &str,
    job_name: &str,
    namespace: &str,
    from: &str,
    command: &[String],
) -> Result<String> {
    use serde_json::{json, Value};

    let cronjob: Value = serde_json::from_str(cronjob_json)?;

    let pod_spec = cronjob
        .pointer("/spec/jobTemplate/spec/template/spec")
        .ok_or_else(|| {
            FirestreamError::ConfigError(
                "CronJob JSON missing spec.jobTemplate.spec.template.spec".into(),
            )
        })?;
    let mut pod_spec = pod_spec.clone();

    let mut container = pod_spec
        .get("containers")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .cloned()
        .ok_or_else(|| {
            FirestreamError::ConfigError("CronJob pod spec has no containers".into())
        })?;

    // Inherit env, drop the backup-only PGDUMP_DIR, append the restore key.
    let mut env: Vec<Value> = container
        .get("env")
        .and_then(|e| e.as_array())
        .cloned()
        .unwrap_or_default();
    env.retain(|e| e.get("name").and_then(|n| n.as_str()) != Some("PGDUMP_DIR"));
    env.push(json!({ "name": "S3_BACKUP_KEY", "value": from }));

    let cobj = container.as_object_mut().ok_or_else(|| {
        FirestreamError::ConfigError("CronJob container is not a JSON object".into())
    })?;
    cobj.insert("name".into(), json!("restore"));
    cobj.insert("env".into(), Value::Array(env));
    cobj.insert("command".into(), json!(command));
    // The CronJob sets `command` (not `args`); strip any inherited args so the
    // restore script is the sole entrypoint.
    cobj.remove("args");

    let pobj = pod_spec.as_object_mut().ok_or_else(|| {
        FirestreamError::ConfigError("CronJob pod spec is not a JSON object".into())
    })?;
    pobj.insert("restartPolicy".into(), json!("Never"));
    pobj.insert("containers".into(), json!([container]));

    let labels = json!({
        "app.kubernetes.io/managed-by": "firestream",
        "app.kubernetes.io/component": "restore",
    });

    let job = json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": job_name,
            "namespace": namespace,
            "labels": labels,
        },
        "spec": {
            "backoffLimit": 0,
            "template": {
                "metadata": { "labels": labels },
                "spec": pod_spec,
            },
        },
    });

    Ok(serde_json::to_string_pretty(&job)?)
}

#[cfg(test)]
mod backup_restore_tests {
    use super::*;
    use serde_json::json;

    fn sample_cronjob() -> serde_json::Value {
        json!({
            "apiVersion": "batch/v1",
            "kind": "CronJob",
            "metadata": { "name": "postgresql-pgdumpall", "namespace": "postgresql" },
            "spec": { "jobTemplate": { "spec": { "template": { "spec": {
                "restartPolicy": "Never",
                "imagePullSecrets": [{ "name": "regcred" }],
                "containers": [{
                    "name": "postgresql-pgdumpall",
                    "image": "registry.example/firestream-postgresql:17",
                    "command": ["bash", "-c", "pg_dumpall | gzip | aws s3 cp - ..."],
                    "env": [
                        { "name": "PGUSER", "value": "postgres" },
                        { "name": "PGHOST", "value": "postgresql" },
                        { "name": "PGPORT", "value": "5432" },
                        { "name": "PGDUMP_DIR", "value": "/backup/pgdump" },
                        { "name": "PGPASSWORD_FILE", "value": "/opt/firestream/postgresql/secrets/postgres-password" },
                        { "name": "AWS_ACCESS_KEY_ID", "valueFrom": { "secretKeyRef": { "name": "cloud-creds", "key": "access" } } },
                        { "name": "S3_ENDPOINT_URL", "value": "" },
                        { "name": "S3_BACKUP_BUCKET", "value": "firestream" },
                        { "name": "S3_ADDRESSING_STYLE", "value": "" }
                    ],
                    "securityContext": { "runAsNonRoot": true },
                    "volumeMounts": [
                        { "name": "postgresql-password", "mountPath": "/opt/firestream/postgresql/secrets/" }
                    ]
                }],
                "volumes": [
                    { "name": "postgresql-password", "secret": { "secretName": "postgresql" } }
                ]
            }}}}}
        })
    }

    #[test]
    fn restore_job_inherits_cronjob_container() {
        let cj = sample_cronjob().to_string();
        let out = build_restore_job_json(
            &cj,
            "postgresql-pgrestore-abc",
            "postgresql",
            "pg-backups/pg_dumpall-2026-01-01-00-00-00.sql.gz",
            &restore_command("postgresql"),
        )
        .expect("build restore job");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(v["kind"], "Job");
        assert_eq!(v["metadata"]["name"], "postgresql-pgrestore-abc");
        assert_eq!(v["spec"]["template"]["spec"]["restartPolicy"], "Never");

        let c = &v["spec"]["template"]["spec"]["containers"][0];
        // Inherited image + securityContext + imagePullSecrets/volumes preserved.
        assert_eq!(c["image"], "registry.example/firestream-postgresql:17");
        assert_eq!(c["securityContext"]["runAsNonRoot"], true);
        assert_eq!(
            v["spec"]["template"]["spec"]["imagePullSecrets"][0]["name"],
            "regcred"
        );
        assert_eq!(
            v["spec"]["template"]["spec"]["volumes"][0]["secret"]["secretName"],
            "postgresql"
        );
        // Command replaced with the restore script.
        assert_eq!(c["command"][0], "bash");
        assert!(c["command"][2].as_str().unwrap().contains("restore complete"));

        // env: PGDUMP_DIR dropped, S3_BACKUP_KEY appended, secretKeyRef cred kept.
        let env = c["env"].as_array().unwrap();
        assert!(env.iter().all(|e| e["name"] != "PGDUMP_DIR"));
        assert!(env.iter().any(|e| e["name"] == "S3_BACKUP_KEY"
            && e["value"] == "pg-backups/pg_dumpall-2026-01-01-00-00-00.sql.gz"));
        assert!(env.iter().any(|e| e["name"] == "AWS_ACCESS_KEY_ID"
            && e["valueFrom"]["secretKeyRef"]["name"] == "cloud-creds"));
        assert!(env.iter().any(|e| e["name"] == "PGHOST" && e["value"] == "postgresql"));
    }

    #[test]
    fn restore_job_errors_on_missing_pod_spec() {
        let bogus = json!({ "kind": "CronJob", "spec": {} }).to_string();
        assert!(build_restore_job_json(&bogus, "j", "ns", "key", &restore_command("postgresql")).is_err());
    }

    #[test]
    fn odoo_restore_command_delegates_to_image_helper() {
        let cj = sample_cronjob().to_string();
        let out = build_restore_job_json(&cj, "odoo-restore-abc", "odoo", "odoo-backups/x.tar.gz", &restore_command("odoo"))
            .expect("build restore job");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let c = &v["spec"]["template"]["spec"]["containers"][0];
        let script = c["command"][2].as_str().unwrap();
        assert!(script.contains("libhelpersodoo.sh"));
        assert!(script.contains("odoo_restore_s3"));
        assert!(!script.contains("psql"));
    }

    fn manifest(name: &str, backup: Option<firestream_charts::Backup>) -> firestream_charts::ChartManifest {
        let json = serde_json::json!({
            "schemaVersion": "1", "name": name, "chart": name, "version": "0.0.0",
            "release": { "releaseName": name, "namespace": name },
        });
        let mut m: firestream_charts::ChartManifest = serde_json::from_value(json).unwrap();
        m.backup = backup;
        m
    }

    #[test]
    fn backup_target_reads_suffix_and_quiesce_from_manifest() {
        let m = manifest("odoo", Some(firestream_charts::Backup {
            cron_job_suffix: "odoodump".into(),
            quiesce_deployment: true,
        }));
        let t = BackupTarget::from_manifest(&m, None).unwrap();
        assert_eq!(t.cronjob, "odoo-odoodump");
        assert_eq!(t.namespace, "odoo");
        assert!(t.quiesce_deployment);
        assert_eq!(t.app_pod_selector(), "app.kubernetes.io/name=odoo,app.kubernetes.io/instance=odoo");

        let t = BackupTarget::from_manifest(&m, Some("erp".into())).unwrap();
        assert_eq!(t.namespace, "erp");
    }

    #[test]
    fn backup_target_rejects_chart_without_backup_contract() {
        let m = manifest("redis", None);
        assert!(BackupTarget::from_manifest(&m, None).is_err());
    }
}
