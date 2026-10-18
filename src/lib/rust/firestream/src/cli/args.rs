//! Command-line argument definitions
//!
//! This module defines the CLI structure using clap.

use clap::{Parser, Subcommand};
use std::path::PathBuf;

/// Firestream CLI - Data Infrastructure Management Tool
#[derive(Parser, Debug)]
#[command(name = "firestream")]
#[command(about = "CLI/TUI tool for managing data infrastructure services", long_about = None)]
#[command(version)]
pub struct Cli {
    /// Path to global config file
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    /// Kubernetes namespace
    #[arg(short, long, global = true)]
    pub namespace: Option<String>,

    /// Increase verbosity
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Suppress output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Output as JSON
    #[arg(long, global = true)]
    pub json: bool,

    /// Path to the Firestream chart bundle (the symlink farm produced by
    /// `nix build .#firestream-charts-bundle`). Mirrors the
    /// `firestream-healthd` pattern of a clap-level default + env override.
    #[arg(
        long,
        global = true,
        env = "FIRESTREAM_CHARTS_DIR",
        default_value = "/opt/firestream/charts"
    )]
    pub charts_dir: PathBuf,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Initialize a new Firestream project
    Init {
        /// Project name
        #[arg(long)]
        name: Option<String>,
    },
    
    /// Generate execution plan
    Plan {
        /// Target specific resources
        #[arg(long)]
        target: Vec<String>,
        
        /// Output format (text, json)
        #[arg(long, default_value = "text")]
        output: String,
        
        /// Save plan to file
        #[arg(long)]
        out: Option<PathBuf>,
    },
    
    /// Apply changes from plan
    Apply {
        /// Plan ID or file
        #[arg(long)]
        plan: Option<String>,
        
        /// Auto-approve changes
        #[arg(long)]
        auto_approve: bool,
        
        /// Target specific resources
        #[arg(long)]
        target: Vec<String>,
    },
    
    /// Refresh state from actual resources
    Refresh,
    
    /// Import existing resources
    Import {
        /// Resource type (infrastructure, build, deployment, cluster)
        resource_type: String,
        
        /// Resource ID
        resource_id: String,
        
        /// Resource data file (JSON)
        #[arg(long)]
        data: Option<PathBuf>,
    },
    
    /// Manage state
    State {
        #[command(subcommand)]
        command: StateCommand,
    },
    
    /// Install a service
    Install {
        /// Service name
        service: String,
        /// Custom configuration file
        #[arg(long)]
        config: Option<PathBuf>,
    },
    
    /// Uninstall a service
    Uninstall {
        /// Service name
        service: String,
    },
    
    /// Start a service
    Start {
        /// Service name
        service: String,
    },
    
    /// Stop a service
    Stop {
        /// Service name
        service: String,
    },
    
    /// Restart a service
    Restart {
        /// Service name
        service: String,
    },
    
    /// Show status of services
    Status {
        /// Service name (shows all if not specified)
        service: Option<String>,
    },
    
    /// List services
    List {
        /// Show available services instead of installed
        #[arg(long)]
        available: bool,
    },
    
    /// Configuration management
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    
    /// View service logs
    Logs {
        /// Service name
        service: String,
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show
        #[arg(short, long, default_value = "100")]
        lines: usize,
    },
    
    /// Show resource usage
    Resources {
        /// Service name (shows all if not specified)
        service: Option<String>,
    },
    
    /// Launch the TUI interface
    #[command(name = "tui")]
    Tui,
    
    /// Manage Kubernetes clusters
    Cluster {
        #[command(subcommand)]
        command: ClusterCommand,
    },
    
    /// Build container images via Nix (auto-detects native vs Docker strategy)
    Build {
        /// Package names to build (e.g., "odoo-15", "postgresql-17")
        #[arg(required = true)]
        packages: Vec<String>,

        /// Force native Nix build (Linux with Nix only)
        #[arg(long)]
        native: bool,

        /// Force Docker-based build
        #[arg(long)]
        docker: bool,

        /// Max parallel builds (default: number of CPUs)
        #[arg(long, short = 'j')]
        parallel: Option<usize>,

        /// Build timeout in seconds
        #[arg(long, default_value = "600")]
        timeout: u64,
    },

    /// Deploy Helm charts driven by the Nix-emitted chart bundle.
    ///
    /// `helm deploy <chart>` resolves a single chart manifest via
    /// `FIRESTREAM_CHARTS_DIR` and runs it through the lifecycle executor.
    /// `helm deploy-stack <stack>` does the same for every chart in a named
    /// stack (e.g. `dev`), skipping entries that aren't yet registered.
    Helm {
        #[command(subcommand)]
        command: HelmCommand,
    },

    /// Deploy/test/demo an app against docker or kubernetes.
    ///
    /// `app list` enumerates the eight canonical Firestream apps and
    /// their backend / demo-data support. The remaining subcommands
    /// (`up`/`down`/`health`/`test`/`status`) drive the docker or k8s
    /// backend; their orchestration is filled in by a later phase.
    App {
        #[command(subcommand)]
        command: AppCommand,
    },

    /// Generate a new project from templates
    Template {
        /// Project name
        #[arg(long)]
        name: Option<String>,
        
        /// Project type (python-fastapi, helm, kubernetes, docker)
        #[arg(long, default_value = "python-fastapi")]
        project_type: String,
        
        /// Output directory
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
        
        /// Non-interactive mode with defaults
        #[arg(long)]
        non_interactive: bool,
        
        /// Values file for non-interactive mode
        #[arg(long)]
        values: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ClusterCommand {
    /// Create a new k3d cluster
    Create {
        /// Cluster name
        #[arg(long)]
        name: Option<String>,
        
        /// Use configuration from file
        #[arg(short, long)]
        config: Option<PathBuf>,
        
        /// Clean mode - delete existing cluster first
        #[arg(long)]
        clean: bool,
        
        /// Number of server nodes
        #[arg(long, default_value = "1")]
        servers: u32,
        
        /// Number of agent nodes
        #[arg(long, default_value = "1")]
        agents: u32,
        
        /// Enable development mode with port forwarding
        #[arg(long)]
        dev_mode: bool,
    },
    
    /// Delete k3d cluster
    Delete {
        /// Cluster name
        name: Option<String>,
    },
    
    /// Show cluster info
    Info {
        /// Cluster name
        name: Option<String>,
    },
    
    /// Setup port forwarding for development
    PortForward {
        /// Service name (or "all" for all services)
        #[arg(default_value = "all")]
        service: String,
        
        /// Port offset for external ports
        #[arg(long, default_value = "10000")]
        offset: u16,
    },
    
    /// View logs from Kubernetes resources
    Logs {
        /// Resource type (pod, deployment, service, etc.)
        #[arg(default_value = "pod")]
        resource_type: String,
        
        /// Resource name (optional, shows all if not specified)
        resource_name: Option<String>,
        
        /// Namespace
        #[arg(short, long, default_value = "default")]
        namespace: String,
        
        /// Follow log output
        #[arg(short, long)]
        follow: bool,
        
        /// Number of lines to show from the end
        #[arg(long, default_value = "100")]
        tail: u32,
        
        /// Show logs from all containers
        #[arg(long)]
        all_containers: bool,
        
        /// Show previous container logs
        #[arg(long)]
        previous: bool,
    },
    
    /// Get diagnostic information from the cluster
    Diagnostics {
        /// Show all diagnostics
        #[arg(long)]
        all: bool,
        
        /// Show node information
        #[arg(long)]
        nodes: bool,
        
        /// Show pod information
        #[arg(long)]
        pods: bool,
        
        /// Show service information
        #[arg(long)]
        services: bool,
        
        /// Show events
        #[arg(long)]
        events: bool,
        
        /// Namespace (use "all" for all namespaces)
        #[arg(short, long, default_value = "all")]
        namespace: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum HelmCommand {
    /// Deploy a single chart by Firestream name (from the index).
    Deploy {
        /// Chart name as registered in `index.json` (e.g. `airflow`).
        chart: String,

        /// Kubernetes namespace override. Falls back to the manifest's
        /// `release.namespace`, then to `default`.
        #[arg(long)]
        namespace: Option<String>,

        /// Pass `--dry-run` to the underlying `helm upgrade --install`.
        #[arg(long)]
        dry_run: bool,
    },

    /// Deploy every chart in a named stack, in declaration order.
    DeployStack {
        /// Stack name as registered in `index.json` (e.g. `dev`).
        stack: String,

        /// Kubernetes namespace override applied to every chart in the
        /// stack. Falls back to per-chart `release.namespace` then to
        /// `default`.
        #[arg(long)]
        namespace: Option<String>,

        /// Pass `--dry-run` to the underlying `helm upgrade --install` for
        /// each chart.
        #[arg(long)]
        dry_run: bool,
    },

    /// Run an on-demand backup of a chart now.
    ///
    /// Creates a one-shot Job from the chart's backup CronJob (named by the
    /// manifest's `backup.cronJobSuffix`, e.g. `<release>-pgdumpall` for
    /// postgresql or `<release>-odoodump` for odoo) and waits for it to
    /// complete. The Job uploads the dump to SeaweedFS S3.
    Backup {
        /// Chart name as registered in `index.json` (e.g. `postgresql`, `odoo`).
        chart: String,

        /// Kubernetes namespace override. Falls back to the manifest's
        /// `release.namespace`, then to `default`.
        #[arg(long)]
        namespace: Option<String>,
    },

    /// Restore a chart's backup from SeaweedFS S3.
    ///
    /// Renders a one-shot Job from the chart's backup CronJob that pulls the
    /// chosen object out of S3 and loads it. PostgreSQL streams the dump into
    /// the running primary. Odoo stops the Deployment first (the restore
    /// replaces the database and the filestore), runs the Job, then scales
    /// the Deployment back and waits for it to be ready.
    Restore {
        /// Chart name as registered in `index.json` (e.g. `postgresql`, `odoo`).
        chart: String,

        /// S3 object key (relative to the backup bucket) to restore from,
        /// e.g. `pg-backups/pg_dumpall-2026-06-28-12-00-00.sql.gz` or
        /// `odoo-backups/odoo-firestream_odoo-2026-06-28-12-00-00.tar.gz`.
        #[arg(long)]
        from: String,

        /// Kubernetes namespace override. Falls back to the manifest's
        /// `release.namespace`, then to `default`.
        #[arg(long)]
        namespace: Option<String>,
    },
}

/// Target backend for an `app` subcommand.
///
/// `docker` drives the Nix-emitted compose stack (`.#<app>-compose` /
/// `.#<app>-up`); `k8s` drives the chart bundle through the helm
/// lifecycle against a (host or ephemeral) k3d cluster.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Backend {
    /// Local docker / docker-compose backend (default).
    #[default]
    Docker,
    /// Kubernetes (k3d) backend.
    K8s,
}

#[derive(Subcommand, Debug)]
pub enum AppCommand {
    /// Bring an app up (deploy + wait for readiness).
    Up {
        /// App name (one of the canonical apps) or the literal `all`.
        app: String,

        /// Backend to deploy against.
        #[arg(long, value_enum, default_value_t = Backend::Docker)]
        backend: Backend,

        /// Force demo/example data ON (overrides the app default).
        #[arg(long)]
        demo: bool,

        /// Force demo/example data OFF (overrides the app default).
        #[arg(long)]
        no_demo: bool,

        /// Block until the app reports healthy.
        #[arg(long)]
        wait: bool,

        /// Readiness/operation timeout in seconds.
        #[arg(long, default_value = "300")]
        timeout: u64,

        /// (k8s only) Create an ephemeral per-run k3d cluster instead of
        /// using the ambient/host cluster.
        #[arg(long)]
        ephemeral: bool,

        /// Kubernetes namespace override (k8s backend).
        #[arg(long)]
        namespace: Option<String>,

        /// (k8s only) Skip preloading the chart's `firestream-*` images
        /// into the target cluster's containerd before the helm deploy.
        /// By default `up --backend k8s` builds + side-loads the images so
        /// pods don't `ImagePullBackOff`. Also disabled via
        /// `FIRESTREAM_APP_PRELOAD=0`.
        #[arg(long)]
        no_preload: bool,
    },

    /// Tear an app down.
    Down {
        /// App name or the literal `all`.
        app: String,

        /// Backend the app was deployed against.
        #[arg(long, value_enum, default_value_t = Backend::Docker)]
        backend: Backend,

        /// (k8s only) Also delete the ephemeral cluster created for the
        /// app, if any.
        #[arg(long)]
        ephemeral: bool,

        /// Kubernetes namespace override (k8s backend).
        #[arg(long)]
        namespace: Option<String>,
    },

    /// Probe an app's health.
    Health {
        /// App name or the literal `all`.
        app: String,

        /// Backend to probe.
        #[arg(long, value_enum, default_value_t = Backend::Docker)]
        backend: Backend,

        /// Probe timeout in seconds.
        #[arg(long, default_value = "300")]
        timeout: u64,

        /// Kubernetes namespace override (k8s backend).
        #[arg(long)]
        namespace: Option<String>,
    },

    /// Up + health (+ optional demo data) as a single test cycle.
    Test {
        /// App name or the literal `all`.
        app: String,

        /// Backend to test against.
        #[arg(long, value_enum, default_value_t = Backend::Docker)]
        backend: Backend,

        /// Force demo/example data ON (overrides the app default).
        #[arg(long)]
        demo: bool,

        /// Force demo/example data OFF (overrides the app default).
        #[arg(long)]
        no_demo: bool,

        /// Block until the app reports healthy (default: true).
        #[arg(long, default_value = "true")]
        wait: bool,

        /// Timeout in seconds for each step.
        #[arg(long, default_value = "300")]
        timeout: u64,

        /// (k8s only) Create an ephemeral per-run k3d cluster.
        #[arg(long)]
        ephemeral: bool,

        /// Kubernetes namespace override (k8s backend).
        #[arg(long)]
        namespace: Option<String>,

        /// (k8s only) Skip preloading the chart's `firestream-*` images
        /// into the target cluster's containerd before the helm deploy.
        /// Also disabled via `FIRESTREAM_APP_PRELOAD=0`.
        #[arg(long)]
        no_preload: bool,
    },

    /// Show the status of deployed apps.
    Status {
        /// Restrict status to a single backend (shows both if omitted).
        #[arg(long, value_enum)]
        backend: Option<Backend>,
    },

    /// List the canonical apps and their backend / demo support.
    List,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Show configuration
    Show {
        /// Service name
        service: String,
    },
    
    /// Edit configuration
    Edit {
        /// Service name
        service: String,
    },
    
    /// Validate configuration
    Validate {
        /// Service name or config file path
        target: String,
    },
}

#[derive(Subcommand, Debug)]
pub enum StateCommand {
    /// Show current state
    Show {
        /// Show specific resource
        #[arg(long)]
        resource: Option<String>,
        
        /// Output format (text, json)
        #[arg(long, default_value = "text")]
        output: String,
    },
    
    /// Lock state
    Lock {
        /// Lock timeout in seconds
        #[arg(long, default_value = "300")]
        timeout: u64,
    },
    
    /// Unlock state
    Unlock {
        /// Force unlock
        #[arg(long)]
        force: bool,
    },
    
    /// List state locks
    Locks,
    
    /// Pull remote state
    Pull,

    /// Push local state to remote
    Push,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_build_command_parse() {
        let cli = Cli::try_parse_from(["firestream", "build", "odoo-15", "postgresql-17"]).unwrap();
        match cli.command {
            Some(Command::Build { packages, native, docker, parallel, timeout }) => {
                assert_eq!(packages, vec!["odoo-15", "postgresql-17"]);
                assert!(!native);
                assert!(!docker);
                assert!(parallel.is_none());
                assert_eq!(timeout, 600);
            }
            _ => panic!("Expected Build command"),
        }
    }

    #[test]
    fn test_build_command_with_flags() {
        let cli = Cli::try_parse_from([
            "firestream", "build", "--docker", "--timeout", "300", "-j", "2", "airflow"
        ]).unwrap();
        match cli.command {
            Some(Command::Build { packages, native, docker, parallel, timeout }) => {
                assert_eq!(packages, vec!["airflow"]);
                assert!(!native);
                assert!(docker);
                assert_eq!(parallel, Some(2));
                assert_eq!(timeout, 300);
            }
            _ => panic!("Expected Build command"),
        }
    }

    #[test]
    fn test_build_command_requires_packages() {
        let result = Cli::try_parse_from(["firestream", "build"]);
        assert!(result.is_err(), "build with no packages should fail");
    }

    #[test]
    fn test_no_command_defaults_to_none() {
        let cli = Cli::try_parse_from(["firestream"]).unwrap();
        assert!(cli.command.is_none());
    }
}
