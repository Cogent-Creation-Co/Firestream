// E2E harness orchestration (docker-compose backend): per-stack lifecycle,
// skip/filter gates, serialisation, port discovery via `nix eval --json`,
// retry-with-deadline, and the StackGuard whose Drop runs `nix run .#<name>-down`
// under a 60s wall-clock timeout (audit correction 6b).
//
// As of Phase 1 of the k3d/helm e2e plan, the transport-agnostic primitives
// (Probe trait, retry loop, env accessors, drop-budget wait helper, Exec
// trait) have moved to `firestream-e2e-core`. This file is the
// docker-specific harness that wraps them.
//
// Probes are SYNCHRONOUS (see `probes.rs`). No tokio runtime is created in
// run_one. This eliminates the "runtime dropped in unwind" hazard the audit
// flagged: there is nothing async to drop, ever.
//
// ------------------------------------------------------------------
// Environment-variable contract (Phase 4)
// ------------------------------------------------------------------
//
// | Var                            | Meaning                                                                           | Default          |
// |--------------------------------|-----------------------------------------------------------------------------------|------------------|
// | `FIRESTREAM_E2E_STACKS`        | CSV subset of canonical stack names; `all` ⇒ every key                            | canonical set    |
// | `FIRESTREAM_E2E_KEEP`          | `1` ⇒ skip teardown and leak check; print `make e2e-sweep RUN=<id>`              | unset            |
// | `FIRESTREAM_E2E_STRICT`        | `1` ⇒ skip-gate failure (no nix/docker) becomes a hard panic                      | unset            |
// | `FIRESTREAM_E2E_TIMEOUT_SECS`  | Per-stack readiness deadline (does NOT bound build time)                          | `300`            |
// | `FIRESTREAM_E2E_PREBUILD`      | `1` ⇒ parallel `nix run .#<image>-image -- --load` outside the global lock        | unset            |
// | `FIRESTREAM_E2E_HEALTHD`       | `0` ⇒ skip the unified HealthEndpoint chain; use per-protocol probes (Phase 1)    | `1` (use it)     |
// | `FIRESTREAM_E2E_HTTP`          | `0` ⇒ skip every HTTP-based probe (HealthEndpoint, HttpReady, SparkWithWorkers)   | `1`              |
//
// `STACKS`, `KEEP`, `STRICT`, `TIMEOUT_SECS` are read via
// `firestream_e2e_core::env`; `PREBUILD`, `HEALTHD`, `HTTP` remain
// docker-harness-local because the next backend (k8s, Phase 2+) uses
// different mechanisms for image preload and health-endpoint discovery.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use firestream_e2e_core::env::{
    docker_daemon_check, env_keep, env_strict, env_timeout_secs, selected, should_skip,
};
use firestream_e2e_core::cleanup::{self, Registration};
use firestream_e2e_core::guard::{TEARDOWN_BUDGET, wait_with_budget};
use firestream_e2e_core::leak;
use firestream_e2e_core::lock::HostLock;
use firestream_e2e_core::retry::retry_until_sync;
use firestream_e2e_core::run_id::RunId;

use crate::probes::{self, Probe, ProbeCtx};
use crate::stacks;

// ----- Global serialisation -----

/// All canonical e2e stacks share a single global lock so they never race for
/// host ports or for the docker daemon's image-load step. Correctness no
/// longer depends on `--test-threads=1`, but the Makefile target still passes
/// it so cargo's per-test output isn't interleaved.
fn e2e_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Optional prebuild: runs every selected stack's image-build list under
/// `nix run .#<image>-image -- --load` *outside* the global lock, populating
/// the per-arch Nix store volume so the locked `up` phase is fast.
///
/// Strictly an opt-in optimisation for warm-cache reruns. Default off — the
/// audited mutex semantics keep the locked-up phase race-free either way.
fn prebuild_once_if_enabled(stack: &str, build_list: &[String]) {
    if std::env::var("FIRESTREAM_E2E_PREBUILD").ok().as_deref() != Some("1") {
        return;
    }
    static ONCE: OnceLock<Mutex<std::collections::BTreeSet<String>>> = OnceLock::new();
    let lock = ONCE.get_or_init(|| Mutex::new(std::collections::BTreeSet::new()));
    let mut seen = lock.lock().unwrap_or_else(|p| p.into_inner());
    for image in build_list {
        if seen.contains(image) {
            continue;
        }
        eprintln!("[e2e:{}] PREBUILD: nix run .#{}-image -- --load", stack, image);
        let status = Command::new("nix")
            .args(["run", &format!(".#{}-image", image), "--", "--load"])
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status();
        match status {
            Ok(s) if s.success() => {
                seen.insert(image.clone());
            }
            Ok(s) => {
                eprintln!("[e2e:{}] PREBUILD image {}: nix run exited {}", stack, image, s);
            }
            Err(e) => {
                eprintln!("[e2e:{}] PREBUILD image {}: nix run spawn failed: {}", stack, image, e);
            }
        }
    }
}

// ----- Skip / strict gates -----

/// Returns Some(reason) if the harness cannot run on this host, else None.
/// `FIRESTREAM_E2E_STRICT=1` upgrades this to a hard panic in `run_one`.
///
/// Two-stage gate:
///   1. `should_skip(&["nix", "docker"])` — both binaries on PATH.
///   2. `docker_daemon_check()` — the daemon answers.
/// Stage 2 lives in core because the docker harness is the only current
/// consumer but the implementation has no docker-specific knowledge beyond
/// "shell out to `docker info`".
fn skip_reason() -> Option<String> {
    if let Some(r) = should_skip(&["nix", "docker"]) {
        return Some(r);
    }
    docker_daemon_check()
}

// ----- Discovery via `nix eval --json` (audit correction 5) -----

/// Resolved per-stack info read from Nix.
struct StackInfo {
    /// Path to the rendered docker-compose.yml (`<store>/docker-compose.yml`).
    compose_file: PathBuf,
    /// docker compose project name.
    project: String,
    /// Host ports published, from the registry `bin/nix/firestream/ports.nix`.
    host_ports: Vec<u16>,
    /// Host port of each published container port, per compose service.
    port_map: probes::PortMap,
    /// Host port for firestream-healthd. `None` when the stack's container
    /// has `health.enable = false`.
    health_host_port: Option<u16>,
    /// Image build order — this stack + its declared `compose.dependencies`.
    build_list: Vec<String>,
}

fn nix_eval_json(attr: &str) -> anyhow::Result<serde_json::Value> {
    let out = Command::new("nix")
        .args(["eval", "--json", attr])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| anyhow::anyhow!("spawn nix eval {}: {}", attr, e))?;
    if !out.status.success() {
        anyhow::bail!("nix eval {} exited {}", attr, out.status);
    }
    serde_json::from_slice(&out.stdout).map_err(|e| anyhow::anyhow!("parse nix eval {}: {}", attr, e))
}

fn nix_build_compose(stack: &str) -> anyhow::Result<PathBuf> {
    let out = Command::new("nix")
        .args(["build", "--no-link", "--print-out-paths", &format!(".#{}-compose", stack)])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| anyhow::anyhow!("spawn nix build .#{}-compose: {}", stack, e))?;
    if !out.status.success() {
        anyhow::bail!("nix build .#{}-compose exited {}", stack, out.status);
    }
    let store_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if store_path.is_empty() {
        anyhow::bail!("nix build .#{}-compose produced no output path", stack);
    }
    Ok(PathBuf::from(store_path).join("docker-compose.yml"))
}

fn resolve_stack(stack: &str) -> anyhow::Result<StackInfo> {
    // Ports come from option evaluation (no rendered-YAML parsing).
    let ports_v = nix_eval_json(&format!(".#{}-compose.passthru.hostPorts", stack))?;
    let host_ports: Vec<u16> = ports_v
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("hostPorts is not an array"))?
        .iter()
        .filter_map(|v| v.as_u64())
        .filter_map(|n| u16::try_from(n).ok())
        .collect();

    let port_map: probes::PortMap = serde_json::from_value(nix_eval_json(&format!(
        ".#{}-compose.passthru.portMap",
        stack
    ))?)
    .map_err(|e| anyhow::anyhow!("portMap of {}: {}", stack, e))?;

    let project_v = nix_eval_json(&format!(".#{}-compose.passthru.projectName", stack))?;
    let project = project_v
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("projectName is not a string"))?
        .to_string();

    let build_v = nix_eval_json(&format!(".#{}-compose.passthru.buildList", stack))?;
    let build_list: Vec<String> = build_v
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("buildList is not an array"))?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    let health_host_port = match nix_eval_json(&format!(
        ".#{}-compose.passthru.healthHostPort",
        stack
    )) {
        Ok(v) if v.is_null() => None,
        Ok(v) => v.as_u64().and_then(|n| u16::try_from(n).ok()),
        // A failure here means the attr is missing (older flake checkout);
        // treat as "not configured" rather than aborting the test.
        Err(_) => None,
    };

    // Build the compose store path so docker compose -f points at a fixed file.
    let compose_file = nix_build_compose(stack)?;

    Ok(StackInfo {
        compose_file,
        project,
        host_ports,
        port_map,
        health_host_port,
        build_list,
    })
}

// ----- nix run wrappers -----

/// Variable that the `<stack>-up` and `<stack>-down` apps read for the
/// compose project name (`nix/flake-modules/compose.nix`).
const COMPOSE_PROJECT_ENV: &str = "FIRESTREAM_COMPOSE_PROJECT";

/// `down` flags for every harness teardown. The `-down` app itself keeps
/// volumes by default, which is right for a dev stack and wrong for a test.
const FULL_DOWN: [&str; 2] = ["-v", "--remove-orphans"];

fn nix_run_app(app: &str, project: &str, args: &[&str]) -> std::io::Result<std::process::Child> {
    Command::new("nix")
        .args(["run", &format!(".#{}", app)])
        .args(if args.is_empty() { &[][..] } else { &["--"][..] })
        .args(args)
        .env(COMPOSE_PROJECT_ENV, project)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
}

fn nix_run_up(stack: &str, project: &str) -> anyhow::Result<()> {
    let status = nix_run_app(&format!("{}-up", stack), project, &[])
        .and_then(|mut c| c.wait())
        .map_err(|e| anyhow::anyhow!("spawn nix run .#{}-up: {}", stack, e))?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("nix run .#{}-up exited {}", stack, status)
    }
}

// ----- Port preflight -----

/// Containers that publish `port` on the host, as `name (project …)`.
fn port_holders(port: u16) -> Vec<String> {
    let out = Command::new("docker")
        .args([
            "ps",
            "--filter",
            &format!("publish={}", port),
            "--format",
            "{{.Names}} (compose project {{.Label \"com.docker.compose.project\"}})",
        ])
        .stdin(Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect(),
        _ => Vec::new(),
    }
}

/// The processes that listen on `port`, as `name (pid N)`. Empty when `ss`
/// is missing or the socket belongs to another user.
fn process_holders(port: u16) -> Vec<String> {
    let out = Command::new("ss")
        .args(["-Hltnp", &format!("sport = :{}", port)])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(o) = out else { return Vec::new() };
    let text = String::from_utf8_lossy(&o.stdout);
    let mut holders: Vec<String> = text
        .split("((")
        .skip(1)
        .filter_map(|users| {
            let mut fields = users.split(',');
            let name = fields.next()?.trim_matches('"');
            let pid = fields.find_map(|f| f.strip_prefix("pid="))?;
            Some(format!("{} (pid {})", name, pid))
        })
        .collect();
    holders.sort();
    holders.dedup();
    holders
}

/// Every host port of `info` that a container or another process holds,
/// with the holder when docker or `ss` knows it.
fn held_ports(info: &StackInfo) -> Vec<(u16, Vec<String>)> {
    let mut ports = info.host_ports.clone();
    ports.extend(info.health_host_port);
    ports.sort_unstable();
    ports.dedup();
    ports
        .into_iter()
        .filter_map(|port| {
            let mut holders = port_holders(port);
            let bound = std::net::TcpListener::bind(("0.0.0.0", port)).is_err();
            if bound && holders.is_empty() {
                holders = process_holders(port);
            }
            (bound || !holders.is_empty()).then_some((port, holders))
        })
        .collect()
}

/// Fail before any build or `up` if a host port of the stack is taken,
/// usually by a dev stack of the same name.
fn preflight_ports(name: &str, info: &StackInfo) {
    // NOTE: no retry. Every registry port is below 32768, outside the Linux
    // ephemeral range, so an outgoing socket never holds one for a moment;
    // a holder is a real listener.
    let held = held_ports(info);
    if held.is_empty() {
        return;
    }
    let lines: Vec<String> = held
        .iter()
        .map(|(port, holders)| {
            if holders.is_empty() {
                format!("  host port {}: held by a process outside docker (run `ss -ltnp` as root to name it)", port)
            } else {
                format!("  host port {}: held by {}", port, holders.join(", "))
            }
        })
        .collect();
    panic!(
        "[e2e:{name}] port preflight failed: the stack publishes host ports that are already in use.\n{}\n\
         Stop the holder first. For a dev stack: `nix run .#{name}-down` (project {}).",
        lines.join("\n"),
        info.project
    );
}

// ----- StackGuard: full teardown under TEARDOWN_BUDGET -----

/// Tears down one compose project. It registers with the cleanup registry
/// when armed, so SIGINT and SIGTERM tear the project down too.
struct StackGuard {
    registration: Option<Registration>,
}

impl StackGuard {
    /// Arm before `up`, so a failed or interrupted `up` is also torn down.
    /// With `keep` set nothing is registered and the project stays.
    fn arm(name: &str, compose_file: PathBuf, project: String, keep: bool) -> Self {
        if keep {
            eprintln!(
                "[e2e:{}] KEEP=1: the stack stays after the test (docker compose -p {} ps)",
                name, project
            );
            return Self { registration: None };
        }
        let name = name.to_string();
        let label = format!("compose project {}", project);
        let registration =
            cleanup::register(label, move || teardown_project(&name, &compose_file, &project));
        Self {
            registration: Some(registration),
        }
    }

    fn teardown(&mut self) {
        if let Some(r) = self.registration.take() {
            r.run();
        }
    }
}

impl Drop for StackGuard {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// `nix run .#<name>-down -- -v --remove-orphans` for `project`. It falls
/// back to `docker compose … down -v --remove-orphans` if nix fails to
/// spawn, exits non-zero, or passes [`TEARDOWN_BUDGET`].
fn teardown_project(name: &str, compose_file: &Path, project: &str) {
    eprintln!(
        "[e2e:{}] teardown: nix run .#{}-down -- {} (project {})",
        name,
        name,
        FULL_DOWN.join(" "),
        project
    );
    match nix_run_app(&format!("{}-down", name), project, &FULL_DOWN) {
        Ok(mut child) => match wait_with_budget(&mut child, TEARDOWN_BUDGET) {
            Ok(()) if child.try_wait().ok().flatten().is_some_and(|s| s.success()) => {
                eprintln!("[e2e:{}] teardown: nix run completed", name);
                return;
            }
            Ok(()) => eprintln!("[e2e:{}] teardown: nix run failed; falling back", name),
            Err(()) => {
                eprintln!(
                    "[e2e:{}] teardown: nix run passed {:?}; killing it and falling back",
                    name, TEARDOWN_BUDGET
                );
                let _ = child.kill();
                let _ = child.wait();
            }
        },
        Err(e) => eprintln!("[e2e:{}] teardown: nix run spawn failed ({}); falling back", name, e),
    }
    compose_down(name, compose_file, project);
}

fn compose_down(name: &str, compose_file: &Path, project: &str) {
    eprintln!(
        "[e2e:{}] teardown: docker compose -f {} -p {} down {}",
        name,
        compose_file.display(),
        project,
        FULL_DOWN.join(" ")
    );
    let mut child = match Command::new("docker")
        .args(["compose", "-f"])
        .arg(compose_file)
        .args(["-p", project, "down"])
        .args(FULL_DOWN)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[e2e:{}] teardown: docker compose spawn failed ({}); giving up", name, e);
            return;
        }
    };
    if wait_with_budget(&mut child, TEARDOWN_BUDGET).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        eprintln!(
            "[e2e:{}] teardown: docker compose down passed {:?}; the leak check reports what is left",
            name, TEARDOWN_BUDGET
        );
    }
}

// ----- Public entry: run_one -----

/// Drive one canonical stack: skip gate, filter gate, locks, resolve, port
/// preflight, pre-clean, arm guard, up, probes, teardown, leak check.
pub fn run_one(name: &str) {
    if let Some(reason) = skip_reason() {
        if env_strict() {
            panic!("[e2e:{}] STRICT: {}", name, reason);
        } else {
            eprintln!("[e2e:{}] SKIP: {}", name, reason);
            return;
        }
    }

    // `selected()` is generic over the canonical-set predicate; the docker
    // harness passes its own `is_canonical`.
    if !selected(name, stacks::is_canonical) {
        eprintln!("[e2e:{}] SKIP: not selected by FIRESTREAM_E2E_STACKS", name);
        return;
    }

    // NOTE: the in-process lock comes first. HostLock is re-entrant inside
    // one process, so it alone would not serialise this binary's tests.
    let _serialize = e2e_lock().lock().unwrap_or_else(|p| p.into_inner());
    let _host = HostLock::acquire().unwrap_or_else(|e| panic!("[e2e:{}] host lock: {:?}", name, e));
    if let Err(e) = cleanup::install_signal_handler() {
        panic!("[e2e:{}] {:?}", name, e);
    }

    eprintln!("[e2e:{}] resolving via `nix eval --json` …", name);
    let info = match resolve_stack(name) {
        Ok(i) => i,
        Err(e) => panic!("[e2e:{}] resolve_stack failed: {:?}", name, e),
    };
    let run = RunId::current();
    let project = run.resource_name(name);
    eprintln!(
        "[e2e:{}] run={} project={} (dev project {}) compose={} host_ports={:?} health_host_port={:?} build_list={:?}",
        name,
        run,
        project,
        info.project,
        info.compose_file.display(),
        info.host_ports,
        info.health_host_port,
        info.build_list
    );

    preflight_ports(name, &info);

    let keep = env_keep();
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| run_stack(name, &info, &project, keep)));
    let leaks = leak::check(run, Some(name), keep);
    match (outcome, leaks) {
        (Ok(()), Ok(())) => {}
        (Ok(()), Err(l)) => panic!("[e2e:{}] {}", name, l),
        (Err(p), leaks) => {
            if let Err(l) = leaks {
                eprintln!("[e2e:{}] {}", name, l);
            }
            std::panic::resume_unwind(p);
        }
    }
}

/// Everything between the preflight and the leak check. The guard drops
/// when this returns or unwinds, so teardown always precedes the leak check.
fn run_stack(name: &str, info: &StackInfo, project: &str, keep: bool) {
    prebuild_once_if_enabled(name, &info.build_list);

    eprintln!("[e2e:{}] pre-clean: docker compose -p {} down {}", name, project, FULL_DOWN.join(" "));
    compose_down(name, &info.compose_file, project);

    let mut guard = StackGuard::arm(name, info.compose_file.clone(), project.to_string(), keep);
    eprintln!("[e2e:{}] starting: nix run .#{}-up (project {})", name, name, project);
    if let Err(e) = nix_run_up(name, project) {
        probes::dump_diagnostics(&info.compose_file, project);
        panic!("[e2e:{}] nix run .#{}-up failed: {:?}", name, name, e);
    }

    // All probes are sync; no tokio runtime is created.
    let deadline = Instant::now() + Duration::from_secs(env_timeout_secs());
    let ctx = ProbeCtx {
        compose_file: info.compose_file.clone(),
        project: project.to_string(),
        host_ports: info.host_ports.clone(),
        port_map: info.port_map.clone(),
        health_host_port: info.health_host_port,
        deadline,
    };
    let probe_chain: Vec<Box<dyn Probe>> = probes::for_stack(name, &ctx);
    if probe_chain.is_empty() {
        eprintln!(
            "[e2e:{}] WARN: empty probe chain (no host_ports?); treating as success",
            name
        );
    }
    for probe in probe_chain.iter() {
        eprintln!("[e2e:{}] probe: {}", name, probe.name());
        if let Err(e) = retry_until_sync(&deadline, || probe.run(&ctx)) {
            let failed = probe.name().to_string();
            probes::dump_diagnostics(&ctx.compose_file, &ctx.project);
            panic!("[e2e:{}] probe '{}' failed: {:?}", name, failed, e);
        }
    }

    eprintln!("[e2e:{}] all probes passed", name);
    guard.teardown();
}
