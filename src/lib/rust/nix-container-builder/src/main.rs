//! CLI for nix-container-builder
//!
//! This binary provides a command-line interface for building containers from Nix flakes.
//!
//! By default, this uses embedded container definitions bundled at compile time,
//! allowing the binary to be portable and run from anywhere with Docker installed.

use clap::{Parser, Subcommand};
use comfy_table::{presets::UTF8_FULL, Table};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use nix_container_builder::{
    BuildConfig, BuildPhase, BuildProgress, NixContainerBuilder, PlatformInfo,
    embedded::{embedded_file_count, list_embedded_files},
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::info;
use tracing_subscriber::EnvFilter;

/// Build Firestream containers using Nix in Docker
///
/// This tool builds container images from embedded Nix flake definitions.
/// It only requires Docker to be installed - Nix is run inside a container.
#[derive(Parser)]
#[command(name = "nix-container-builder")]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,

    /// Path to external containers directory (overrides embedded)
    ///
    /// By default, this tool uses container definitions embedded at compile time.
    /// Use this flag to build from an external directory instead.
    #[arg(long, global = true, env = "CONTAINERS_DIR")]
    containers_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// List available containers
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Build a container
    Build {
        /// Container name (or "all" for all containers)
        name: String,

        /// Force native Nix build (only works on Linux with Nix installed)
        #[arg(long)]
        native: bool,

        /// Force Docker-based build (default for embedded mode)
        #[arg(long)]
        docker: bool,

        /// Build timeout in seconds
        #[arg(long, default_value = "600")]
        timeout: u64,

        /// Number of parallel builds (default: number of CPUs)
        #[arg(long, short = 'j')]
        parallel: Option<usize>,

        /// Disable parallel builds
        #[arg(long)]
        sequential: bool,
    },

    /// Show container information
    Info {
        /// Container name
        name: String,
    },

    /// Show platform information
    Platform,

    /// Show embedded workspace information
    Embedded {
        /// List all embedded files
        #[arg(long)]
        files: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Setup logging
    let filter = if cli.verbose {
        EnvFilter::new("nix_container_builder=debug,info")
    } else {
        EnvFilter::new("nix_container_builder=info,warn")
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    match &cli.command {
        Commands::List { json } => handle_list(&cli, *json).await,
        Commands::Build {
            name,
            native,
            docker,
            timeout,
            parallel,
            sequential,
        } => handle_build(&cli, name, *native, *docker, *timeout, *parallel, *sequential).await,
        Commands::Info { name } => handle_info(&cli, name).await,
        Commands::Platform => handle_platform().await,
        Commands::Embedded { files } => handle_embedded(*files).await,
    }
}

async fn create_builder(cli: &Cli) -> anyhow::Result<NixContainerBuilder> {
    // Use external containers dir if provided, otherwise use embedded mode
    if let Some(ref containers_dir) = cli.containers_dir {
        let config = BuildConfig::default().with_containers_dir(containers_dir);
        NixContainerBuilder::with_config(config).await.map_err(Into::into)
    } else {
        // Default: Use embedded container definitions (portable mode)
        NixContainerBuilder::embedded().await.map_err(Into::into)
    }
}

async fn handle_embedded(show_files: bool) -> anyhow::Result<()> {
    let file_count = embedded_file_count();

    println!("Embedded Workspace Information");
    println!("==============================");
    println!("Total embedded files: {}", file_count);
    println!();

    if show_files {
        println!("Embedded files:");
        for file in list_embedded_files() {
            println!("  {}", file);
        }
        println!();
    }

    // Extract and show available containers
    let builder = NixContainerBuilder::embedded().await?;
    let containers = builder.discover_containers().await?;

    println!("Available containers: {}", containers.len());
    for container in &containers {
        println!("  - {} ({})", container.name, container.version.as_deref().unwrap_or("unknown"));
    }

    if let Some(root) = builder.workspace_root() {
        println!();
        println!("Extracted to: {}", root.display());
    }

    println!();
    println!("Mode: {} (portable)", if builder.is_embedded() { "Embedded" } else { "External" });

    Ok(())
}

async fn handle_list(cli: &Cli, json: bool) -> anyhow::Result<()> {
    let builder = create_builder(cli).await?;
    let containers = builder.discover_containers().await?;

    if json {
        let json_output: Vec<serde_json::Value> = containers
            .iter()
            .map(|c| {
                serde_json::json!({
                    "name": c.name,
                    "path": c.path.display().to_string(),
                    "has_flake": c.has_flake,
                    "has_dockerfile": c.has_dockerfile,
                    "version": c.version,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_output)?);
    } else {
        let mut table = Table::new();
        table.load_preset(UTF8_FULL);
        table.set_header(vec!["Name", "Version", "Flake", "Dockerfile", "Path"]);

        for container in &containers {
            table.add_row(vec![
                container.name.clone(),
                container.version.as_deref().unwrap_or("-").to_string(),
                if container.has_flake { "yes".to_string() } else { "no".to_string() },
                if container.has_dockerfile { "yes".to_string() } else { "no".to_string() },
                container.path.display().to_string(),
            ]);
        }

        println!("{table}");
        println!("\nFound {} containers", containers.len());
    }

    Ok(())
}

async fn handle_build(
    cli: &Cli,
    name: &str,
    native: bool,
    docker: bool,
    timeout: u64,
    parallel: Option<usize>,
    sequential: bool,
) -> anyhow::Result<()> {
    // Strategy selection.
    //
    // This used to be `.with_force_docker(docker || !native)`, which made
    // `--native` the ONLY way to ever reach a native build — even with a real
    // `--containers-dir` pointing at a checked-out repo on a Linux host with a
    // populated /nix/store. That is the wrong default for the external case.
    //
    // Now:
    //   * `--docker` always forces Docker (both modes).
    //   * `--native` forces native.
    //   * neither flag + `--containers-dir` => auto; `create_strategy` probes
    //     the platform (strategy/mod.rs) and picks native on Linux-with-Nix.
    //   * neither flag + embedded => Docker, carried by
    //     `BuildConfig::for_embedded()`'s own `force_docker: true`
    //     (src/config.rs), which is correct there: the portable binary cannot
    //     assume Nix exists on the host it was copied to.
    //
    // Matches `firestream build --native/--docker` semantics.
    let config = BuildConfig::for_embedded()
        .with_force_native(native)
        .with_timeout(Duration::from_secs(timeout));

    // Use external containers dir if provided, otherwise use embedded mode
    let builder = if let Some(ref containers_dir) = cli.containers_dir {
        // External checkout: clear the embedded Docker default; `docker` alone
        // decides, and false means "auto-detect".
        let config = config.with_force_docker(docker).with_containers_dir(containers_dir);
        NixContainerBuilder::with_config(config).await?
    } else {
        // Embedded: keep `for_embedded()`'s force_docker unless it is already true.
        let config = if docker { config.with_force_docker(true) } else { config };
        NixContainerBuilder::embedded_with_config(config).await?
    };

    info!(
        "Using {} mode",
        if builder.is_embedded() { "embedded" } else { "external" }
    );

    // Determine which containers to build
    let names: Vec<String> = if name == "all" {
        let containers = builder.discover_containers().await?;
        containers.into_iter().map(|c| c.name).collect()
    } else {
        vec![name.to_string()]
    };

    let name_refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();

    info!("Building {} container(s)...", names.len());

    // Setup progress display
    let multi_progress = MultiProgress::new();
    let progress_bars: Arc<Mutex<HashMap<String, ProgressBar>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let style = ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] {bar:40.cyan/blue} {msg}")
        .unwrap()
        .progress_chars("##-");

    // Create progress bars for each container
    for name in &names {
        let pb = multi_progress.add(ProgressBar::new(100));
        pb.set_style(style.clone());
        pb.set_message(format!("{}: Waiting...", name));
        progress_bars.lock().await.insert(name.clone(), pb);
    }

    // Progress callback
    let progress_bars_clone = Arc::clone(&progress_bars);
    let progress_callback = move |progress: BuildProgress| {
        let progress_bars = progress_bars_clone.clone();
        tokio::spawn(async move {
            let bars = progress_bars.lock().await;
            if let Some(pb) = bars.get(&progress.container) {
                let msg = match &progress.phase {
                    BuildPhase::Discovering => format!("{}: Discovering...", progress.container),
                    BuildPhase::Resolving => format!("{}: Resolving flake...", progress.container),
                    BuildPhase::Building { current, total } => {
                        format!("{}: Building ({}/{})...", progress.container, current, total)
                    }
                    BuildPhase::Loading => format!("{}: Loading into Docker...", progress.container),
                    BuildPhase::Complete => format!("{}: Complete!", progress.container),
                    BuildPhase::Failed => format!("{}: Failed!", progress.container),
                };
                pb.set_message(msg);

                if let Some(percent) = progress.percent {
                    pb.set_position(percent as u64);
                }

                if progress.phase.is_terminal() {
                    pb.finish();
                }
            }
        });
    };

    // Run builds
    let results = if sequential || names.len() == 1 {
        // Sequential builds
        let mut results = Vec::new();
        for name in &names {
            let result = builder.build(name).await;
            match &result {
                Ok(r) => {
                    let bars = progress_bars.lock().await;
                    if let Some(pb) = bars.get(name) {
                        pb.finish_with_message(format!("{}: {}", name, r.full_ref()));
                    }
                }
                Err(e) => {
                    let bars = progress_bars.lock().await;
                    if let Some(pb) = bars.get(name) {
                        pb.finish_with_message(format!("{}: Error - {}", name, e));
                    }
                }
            }
            results.push(result);
        }
        results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("{}", e))?
    } else {
        // Parallel builds
        let max_concurrent = parallel.unwrap_or(num_cpus::get());
        builder
            .build_parallel_with_progress(&name_refs, Some(max_concurrent), progress_callback)
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?
    };

    // Print summary
    println!("\nBuild Summary:");
    println!("==============");

    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(vec!["Container", "Image", "Duration", "Strategy"]);

    for result in &results {
        table.add_row(vec![
            &result.container,
            &result.full_ref(),
            &format!("{:.1}s", result.duration.as_secs_f64()),
            &result.strategy.to_string(),
        ]);
    }

    println!("{table}");
    println!(
        "\nSuccessfully built {} container(s)",
        results.len()
    );

    Ok(())
}

async fn handle_info(cli: &Cli, name: &str) -> anyhow::Result<()> {
    let builder = create_builder(cli).await?;
    let container = builder.get_container(name).await?;

    println!("Container: {}", container.name);
    println!("Path: {}", container.path.display());
    println!("Version: {}", container.version.as_deref().unwrap_or("unknown"));
    println!("Has flake.nix: {}", container.has_flake);
    println!("Has Dockerfile: {}", container.has_dockerfile);
    println!("Can build with Nix: {}", container.can_build_nix());

    if container.has_flake {
        println!("\nFlake path: {}", container.flake_path().display());
    }

    Ok(())
}

async fn handle_platform() -> anyhow::Result<()> {
    let platform = PlatformInfo::detect().await?;

    println!("Platform Information:");
    println!("====================");
    println!("OS: {}", platform.platform);
    println!("Architecture: {}", platform.arch);
    println!("Nix available: {}", platform.nix_available);
    println!("Docker available: {}", platform.docker_available);
    println!("Running in container: {}", platform.in_container);
    println!();
    println!("Recommended strategy: {}", platform.recommended_strategy());
    println!("Docker platform: {}", platform.docker_platform());
    println!("Nix store volume: {}", platform.default_nix_store_volume());
    println!();
    println!("Can build native: {}", platform.can_build_native());
    println!("Can build docker: {}", platform.can_build_docker());

    Ok(())
}
