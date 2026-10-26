// Per-protocol readiness probes for canonical Firestream stacks (docker-
// compose backend).
//
// As of Phase 1 of the k3d/helm e2e plan, the transport-agnostic primitives
// (`Probe` trait, `Tcp`, `HttpReady`, `PgIsReady`, `RedisPing`,
// `KafkaApiVersions`) have moved to `firestream-e2e-core`. This file is now
// the **docker-specific** probe matrix:
//
//   * `DockerComposeExec` — `Exec` impl that shells out to
//     `docker compose -f <compose_file> -p <project> exec -T <service> <cmd…>`.
//   * `ProbeCtx`           — per-stack context populated from `nix eval --json`.
//   * `SparkWithWorkers`   — REST-JSON probe specific to the Spark master.
//   * `HealthEndpoint`     — `firestream-healthd` `/readyz` + `/sbom` probe.
//   * `for_stack(stack, ctx)` — the per-stack chain matrix the docker harness
//     consults.
//   * `dump_diagnostics`   — `docker compose ps` + `logs --tail 200` on failure.
//
// Audit correction 3: a blanket TCP-is-bound check is near-vacuous for several
// stacks (Kafka's listener binds before metadata is serviceable; a Spark
// master binds before workers register). Each probe below tests the
// protocol-level invariant that downstream consumers actually depend on.
//
// All probes are SYNCHRONOUS. The harness drives them inside a retry loop
// with a wall-clock deadline (see `firestream_e2e_core::retry::retry_until_sync`).

use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};

use firestream_e2e_core::cleanup;
use firestream_e2e_core::exec::Exec;
use firestream_e2e_core::probe::{
    HttpReady, KafkaApiVersions, PgIsReady, RedisPing, Tcp,
};
use firestream_e2e_core::supabase::{self as supabase_probes, SupabaseStack};

// ---------- DockerComposeExec: Exec impl for `docker compose exec -T` ----------

/// `Exec` impl that translates `target` into a compose service inside a fixed
/// `(compose_file, project)` pair and runs the requested command via
/// `docker compose -f <file> -p <project> exec -T <target> <args…>`.
///
/// One instance per stack — stash it in an `Arc` and hand it to every
/// exec-driven probe in the stack's chain.
pub struct DockerComposeExec {
    pub compose_file: PathBuf,
    pub project_name: String,
}

impl Exec for DockerComposeExec {
    fn exec(&self, target: &str, args: &[&str]) -> Result<Output> {
        let mut cmd = Command::new("docker");
        cmd.args(["compose", "-f"])
            .arg(&self.compose_file)
            .args(["-p", &self.project_name, "exec", "-T", target])
            .args(args)
            .stdin(Stdio::null());
        cmd.output()
            .with_context(|| format!("spawn docker compose exec {} {:?}", target, args))
    }
}

// ---------- ProbeCtx ----------

/// Host port of each published container port, per compose service, as the
/// compose package's `passthru.portMap` gives it. JSON object keys are
/// strings, so the container port is a string here.
pub type PortMap = HashMap<String, HashMap<String, u16>>;

/// Per-probe context populated by the docker harness from `nix eval` data.
pub struct ProbeCtx {
    /// Path to the rendered docker-compose.yml (for `docker compose -f`).
    pub compose_file: PathBuf,
    /// docker compose project name (-p).
    pub project: String,
    /// Host ports actually published by this stack.
    pub host_ports: Vec<u16>,
    /// Host port of each published container port, per compose service.
    pub port_map: PortMap,
    /// Host port for the in-image firestream-healthd service, from
    /// `passthru.healthHostPort`. `None` when the container has
    /// `health.enable = false`.
    pub health_host_port: Option<u16>,
    /// Wall-clock deadline for the entire readiness phase of this stack.
    /// Allow dead_code: kept for Phase 3+ probes that bound their own
    /// per-probe operations against the outer deadline.
    #[allow(dead_code)]
    pub deadline: Instant,
}

impl ProbeCtx {
    /// The host port that compose `service` publishes for `container`.
    fn host_port(&self, service: &str, container: u16) -> Option<u16> {
        self.port_map.get(service)?.get(&container.to_string()).copied()
    }

    /// Construct a fresh `Arc<dyn Exec>` for the exec-driven probes in this
    /// stack's chain. Cheap — the result wraps a `DockerComposeExec`
    /// which holds only a path and a string.
    fn exec(&self) -> Arc<dyn Exec> {
        Arc::new(DockerComposeExec {
            compose_file: self.compose_file.clone(),
            project_name: self.project.clone(),
        })
    }
}

// ---------- Probe-trait adapter ----------
//
// `firestream_e2e_core::probe::Probe::run` takes `()` (transport-agnostic),
// but the docker harness's run loop calls `probe.run(&ctx)`. Wrap every core
// probe in a thin adapter so the for_stack() return type stays a
// `Vec<Box<dyn LocalProbe>>` and the harness code is unchanged.

/// Local probe trait — same shape as the original `probes::Probe`, takes the
/// docker `ProbeCtx`. Implemented for both:
///   * `CoreProbe<T>`           — adapter for transport-agnostic core probes
///   * `SparkWithWorkers`       — docker-specific (HTTP-based) probe
///   * `HealthEndpoint`         — docker-specific firestream-healthd probe
pub trait Probe_ {
    fn name(&self) -> &str;
    fn run(&self, ctx: &ProbeCtx) -> Result<()>;
}

// Re-export under the historical name so the harness imports stay untouched.
pub use Probe_ as Probe;

/// Adapter from `firestream_e2e_core::probe::Probe` (no ctx) to the harness's
/// local `Probe` trait (takes `&ProbeCtx`).
pub struct CoreProbe<T: firestream_e2e_core::probe::Probe> {
    inner: T,
}

impl<T: firestream_e2e_core::probe::Probe> CoreProbe<T> {
    pub fn new(inner: T) -> Self {
        Self { inner }
    }
}

impl<T: firestream_e2e_core::probe::Probe> Probe_ for CoreProbe<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn run(&self, _ctx: &ProbeCtx) -> Result<()> {
        self.inner.run()
    }
}

/// Convenience: `Box<CoreProbe<T>> as Box<dyn Probe_>` in one call.
fn boxed<T: firestream_e2e_core::probe::Probe + 'static>(p: T) -> Box<dyn Probe_> {
    Box::new(CoreProbe::new(p))
}

// ---------- spark master + worker count (docker-specific) ----------

/// HTTP GET on master REST endpoint, parse JSON, require `aliveworkers >= min_workers`.
/// Reference: Spark master serves `/json/` returning `{ aliveworkers, ... }`.
pub struct SparkWithWorkers {
    pub master_url: String, // e.g. "http://127.0.0.1:8080"
    pub min_workers: u32,
}

impl Probe_ for SparkWithWorkers {
    fn name(&self) -> &str {
        "spark_with_workers"
    }
    fn run(&self, _ctx: &ProbeCtx) -> Result<()> {
        // Reach the UI first (proves the master process is up).
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .context("build reqwest blocking client")?;

        let json_url = format!("{}/json/", self.master_url.trim_end_matches('/'));
        let resp = client.get(&json_url).send().context("GET spark master json")?;
        let status = resp.status().as_u16();
        if !(status < 500) {
            bail!("spark master /json/ status {}", status);
        }
        // Parse via text() + serde_json so we don't pull the `json` feature
        // into reqwest (keeps the dev-dep closure minimal).
        let text = resp.text().context("read spark master json body")?;
        let body: serde_json::Value =
            serde_json::from_str(&text).context("parse spark master json")?;
        // Spark's REST JSON uses "aliveworkers" (lowercase, one word) historically.
        let alive = body
            .get("aliveworkers")
            .and_then(|v: &serde_json::Value| v.as_u64())
            .or_else(|| body.get("aliveWorkers").and_then(|v: &serde_json::Value| v.as_u64()))
            .ok_or_else(|| anyhow!("spark master json missing aliveworkers/aliveWorkers field"))?;
        if (alive as u32) >= self.min_workers {
            Ok(())
        } else {
            Err(anyhow!("spark aliveworkers={} < required {}", alive, self.min_workers))
        }
    }
}

// ---------- firestream-healthd /readyz + /sbom (docker-specific) ----------

/// Probe the in-image `firestream-healthd` service. Polls `<base_url>/readyz`
/// until 200, then makes a single `<base_url>/sbom?format=cyclonedx` request
/// and asserts the document is a JSON object containing a non-empty
/// `components` array. The /sbom check happens once on the first /readyz
/// success — the harness retry loop is only responsible for the readiness
/// portion; a /sbom failure escalates immediately because the SBOM is baked
/// at build time and retries cannot fix a malformed document.
pub struct HealthEndpoint {
    pub base_url: String,
}

impl Probe_ for HealthEndpoint {
    fn name(&self) -> &str {
        "health-endpoint"
    }
    fn run(&self, _ctx: &ProbeCtx) -> Result<()> {
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .context("build reqwest blocking client")?;

        // Step 1: /readyz must return 200.
        let readyz_url = format!("{}/readyz", self.base_url.trim_end_matches('/'));
        let resp = client
            .get(&readyz_url)
            .send()
            .with_context(|| format!("GET {}", readyz_url))?;
        let status = resp.status().as_u16();
        if status != 200 {
            bail!("GET {} status {} (want 200)", readyz_url, status);
        }

        // Step 2: /sbom?format=cyclonedx must parse as an object with a
        // non-empty `components` array.
        let sbom_url = format!(
            "{}/sbom?format=cyclonedx",
            self.base_url.trim_end_matches('/')
        );
        let sbom_resp = client
            .get(&sbom_url)
            .send()
            .with_context(|| format!("/sbom assertion: GET {}", sbom_url))?;
        let sbom_status = sbom_resp.status().as_u16();
        if sbom_status != 200 {
            bail!(
                "/sbom assertion: GET {} status {} (want 200)",
                sbom_url,
                sbom_status
            );
        }
        let body = sbom_resp
            .text()
            .with_context(|| format!("/sbom assertion: read body of {}", sbom_url))?;
        let doc: serde_json::Value = serde_json::from_str(&body)
            .with_context(|| format!("/sbom assertion: parse JSON from {}", sbom_url))?;
        let obj = doc
            .as_object()
            .ok_or_else(|| anyhow!("/sbom assertion: top-level JSON is not an object"))?;
        let components = obj
            .get("components")
            .ok_or_else(|| anyhow!("/sbom assertion: missing `components` field"))?
            .as_array()
            .ok_or_else(|| anyhow!("/sbom assertion: `components` is not an array"))?;
        if components.is_empty() {
            bail!("/sbom assertion: `components` array is empty");
        }
        Ok(())
    }
}

// ---------- Supabase ----------

/// The gateway's healthd, the shared functional probes through the gateway,
/// then the Studio egress capture.
fn supabase_chain(ctx: &ProbeCtx) -> Vec<Box<dyn Probe_>> {
    let mut chain: Vec<Box<dyn Probe_>> = Vec::new();
    if let Some(hp) = ctx.health_host_port {
        chain.push(Box::new(HealthEndpoint {
            base_url: format!("http://127.0.0.1:{}", hp),
        }));
    }
    let stack = Arc::new(SupabaseStack {
        exec: ctx.exec(),
        db_target: "db".into(),
        studio_target: "studio".into(),
        gateway_target: "api-gw".into(),
        gateway_url: format!(
            "http://127.0.0.1:{}",
            ctx.host_port("api-gw", 8000).expect("supabase api-gw publishes 8000")
        ),
        internal_gateway_url: "http://api-gw:8000".into(),
        supavisor_host: "supavisor".into(),
    });
    for p in supabase_probes::probes(stack.clone()) {
        chain.push(boxed(p));
    }
    chain.push(Box::new(StudioEgressCapture { stack }));
    chain
}

/// Captures every packet in the Studio container's network namespace while
/// Studio serves its pages and API routes, then fails if a packet left the
/// compose network or a DNS query named PostHog or ConfigCat.
///
/// tcpdump comes from the flake's nixpkgs and runs in a helper container
/// that joins Studio's network namespace with the host's /nix/store mounted
/// read-only, so the Studio image needs no capture tools. The capture sees
/// the queries to Docker's embedded DNS (127.0.0.11) on loopback, so a
/// lookup of an outside name is visible even when the connection never
/// happens. It covers Studio's server side; requests that a browser makes
/// from Studio's client bundle are outside this capture.
pub struct StudioEgressCapture {
    stack: Arc<SupabaseStack>,
}

fn run_ok(cmd: &mut Command, what: &str) -> Result<String> {
    let out = cmd.stdin(Stdio::null()).output().with_context(|| format!("spawn {}", what))?;
    if !out.status.success() {
        bail!("{} exited {}: {}", what, out.status, String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `a.b.c.d` (or `a.b.c.d.port`) inside `net/prefix`.
fn ipv4_in(addr: &str, net: &str) -> bool {
    let parse = |s: &str| -> Option<u32> {
        let o: Vec<u32> = s.split('.').take(4).map(|p| p.parse().ok()).collect::<Option<_>>()?;
        if o.len() != 4 || o.iter().any(|x| *x > 255) {
            return None;
        }
        Some((o[0] << 24) | (o[1] << 16) | (o[2] << 8) | o[3])
    };
    let Some((base, prefix)) = net.split_once('/') else { return false };
    let (Some(a), Some(b), Ok(prefix)) = (parse(addr), parse(base), prefix.parse::<u32>()) else {
        return false;
    };
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
    a & mask == b & mask
}

/// True for addresses that cannot leave the host's network segment:
/// loopback, link-local and multicast.
fn segment_local(addr: &str, ipv6: bool) -> bool {
    if ipv6 {
        let a = addr.to_ascii_lowercase();
        a.starts_with("::1.") || a.starts_with("fe80:") || a.starts_with("ff")
    } else {
        let first: u32 = addr.split('.').next().and_then(|o| o.parse().ok()).unwrap_or(0);
        first == 127 || (224..=239).contains(&first) || addr.starts_with("169.254.") || addr.starts_with("255.255.255.255")
    }
}

/// Every DNS name queried, and every outbound packet (direction `Out`) whose
/// destination is outside `nets` and not segment-local, from `tcpdump -i any
/// -nn` text output.
fn analyse_capture(text: &str, nets: &[String]) -> (Vec<String>, Vec<String>) {
    let mut names = Vec::new();
    let mut outside = Vec::new();
    for line in text.lines() {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        for (i, t) in tokens.iter().enumerate() {
            if (*t == "A?" || *t == "AAAA?") && i + 1 < tokens.len() {
                names.push(tokens[i + 1].trim_end_matches('.').to_string());
            }
        }
        let Some(i) = tokens.iter().position(|t| *t == "IP" || *t == "IP6") else { continue };
        if i == 0 || tokens[i - 1] != "Out" {
            continue;
        }
        let Some(dst) = tokens.get(i + 3) else { continue };
        let dst = dst.trim_end_matches(':');
        let ipv6 = tokens[i] == "IP6";
        let local = segment_local(dst, ipv6) || (!ipv6 && nets.iter().any(|n| ipv4_in(dst, n)));
        if !local {
            outside.push(line.to_string());
        }
    }
    (names, outside)
}

#[cfg(test)]
mod egress_tests {
    use super::*;

    const NETS: &[&str] = &["172.20.0.5/16"];

    fn nets() -> Vec<String> {
        NETS.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn in_stack_and_multicast_traffic_is_local() {
        let text = "\
06:46:28.1 eth0  Out IP 172.20.0.5.41234 > 172.20.0.9.8080: Flags [S]
06:46:28.2 eth0  In  IP 172.20.0.9.8080 > 172.20.0.5.41234: Flags [S.]
06:46:28.3 eth0  M   IP6 fe80::d851:ecff:fed9:419f.5353 > ff02::fb.5353: 0 PTR (QM)? _ipp._tcp.local.
06:46:28.4 lo    Out IP 127.0.0.1.51000 > 127.0.0.11.53: 1+ A? meta. (22)";
        let (names, outside) = analyse_capture(text, &nets());
        assert_eq!(names, vec!["meta".to_string()]);
        assert!(outside.is_empty(), "{:?}", outside);
    }

    #[test]
    fn outbound_packet_to_the_internet_is_flagged() {
        let text = "\
06:46:29.1 lo    Out IP 127.0.0.1.51000 > 127.0.0.11.53: 2+ A? us.i.posthog.com. (34)
06:46:29.2 eth0  Out IP 172.20.0.5.41300 > 44.205.12.1.443: Flags [S]";
        let (names, outside) = analyse_capture(text, &nets());
        assert_eq!(names, vec!["us.i.posthog.com".to_string()]);
        assert_eq!(outside.len(), 1);
    }
}

/// `docker rm -f -v`: the `-v` also removes the anonymous volumes that the
/// Studio image declares.
fn remove_helper(name: &str) {
    let _ = Command::new("docker")
        .args(["rm", "-f", "-v", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

impl Probe_ for StudioEgressCapture {
    fn name(&self) -> &str {
        "supabase_studio_egress"
    }
    fn run(&self, ctx: &ProbeCtx) -> Result<()> {
        let compose = |args: &[&str]| {
            let mut c = Command::new("docker");
            c.args(["compose", "-f"]).arg(&ctx.compose_file).args(["-p", &ctx.project]).args(args);
            c
        };
        let studio = run_ok(&mut compose(&["ps", "-q", "studio"]), "docker compose ps studio")?;
        let image = run_ok(
            Command::new("docker").args(["inspect", "-f", "{{.Config.Image}}", &studio]),
            "docker inspect studio",
        )?;
        let nets: Vec<String> = run_ok(
            Command::new("docker").args([
                "inspect",
                "-f",
                "{{range .NetworkSettings.Networks}}{{.IPAddress}}/{{.IPPrefixLen}} {{end}}",
                &studio,
            ]),
            "docker inspect studio networks",
        )?
        .split_whitespace()
        .map(String::from)
        .collect();
        let tcpdump = run_ok(
            Command::new("nix").args([
                "build",
                "--inputs-from",
                ".",
                "nixpkgs#tcpdump",
                "--no-link",
                "--print-out-paths",
            ]),
            "nix build nixpkgs#tcpdump",
        )?;
        let tcpdump = format!("{}/bin/tcpdump", tcpdump.lines().last().unwrap_or_default());

        let helper = format!("{}-studio-egress", ctx.project);
        remove_helper(&helper);
        // NOTE: the compose project label makes the helper visible to the
        // leak check and to `firestream-e2e-sweep`, although compose did not
        // create it.
        let registration = {
            let helper = helper.clone();
            cleanup::register(format!("egress helper {}", helper), move || remove_helper(&helper))
        };
        let started = run_ok(
            Command::new("docker").args([
                "run",
                "-d",
                "--rm",
                "--name",
                &helper,
                "--label",
                &format!("com.docker.compose.project={}", ctx.project),
                "--network",
                &format!("container:{}", studio),
                "--user",
                "0",
                "--cap-add",
                "NET_RAW",
                "--cap-add",
                "NET_ADMIN",
                "-v",
                "/nix/store:/nix/store:ro",
                "--entrypoint",
                &tcpdump,
                &image,
                "-i",
                "any",
                "-nn",
                "-l",
            ]),
            "docker run tcpdump helper",
        );
        // NOTE: a failed `docker run -d` can leave a created container, and
        // dropping the registration would only deregister it.
        if let Err(e) = started {
            registration.run();
            return Err(e);
        }
        let result = (|| -> Result<(Vec<String>, Vec<String>, String)> {
            std::thread::sleep(Duration::from_secs(3));
            let pw = self.stack.exec.exec(&self.stack.gateway_target, &["printenv", "DASHBOARD_PASSWORD"])?;
            let pw = String::from_utf8_lossy(&pw.stdout).trim().to_string();
            let client = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()?;
            for path in [
                "/project/default",
                "/project/default/editor",
                "/project/default/auth/users",
                "/project/default/storage/buckets",
                "/api/platform/profile",
                "/api/platform/projects",
                "/api/platform/projects/default/settings",
                "/api/platform/pg-meta/default/tables?included_schemas=public",
            ] {
                let url = format!("{}{}", self.stack.gateway_url, path);
                let status = client.get(&url).basic_auth("supabase", Some(&pw)).send()?.status();
                eprintln!("[e2e:supabase] studio egress capture: GET {} -> {}", path, status);
            }
            std::thread::sleep(Duration::from_secs(10));
            let logs = Command::new("docker").args(["logs", &helper]).stdin(Stdio::null()).output()?;
            let text = String::from_utf8_lossy(&logs.stdout).to_string();
            let (names, outside) = analyse_capture(&text, &nets);
            Ok((names, outside, text))
        })();
        registration.run();
        let (names, outside, text) = result?;

        // NOTE: a capture that missed Studio's own postgres-meta traffic would
        // also miss egress, so it would prove nothing.
        if !text.lines().any(|l| l.contains(".8080:") || l.contains(".8080 ")) {
            bail!("the capture saw no Studio -> postgres-meta traffic; it is not capturing");
        }
        let banned: Vec<&String> = names
            .iter()
            .filter(|n| {
                let n = n.to_ascii_lowercase();
                n.contains("posthog") || n.contains("configcat")
            })
            .collect();
        eprintln!(
            "[e2e:supabase] studio egress capture: {} lines, DNS names {:?}",
            text.lines().count(),
            names
        );
        if !banned.is_empty() || !outside.is_empty() {
            panic!(
                "[e2e:supabase] Studio traffic left the stack: banned DNS names {:?}; \
                 packets outside {:?}: {:?}",
                banned,
                nets,
                outside.iter().take(20).collect::<Vec<_>>()
            );
        }
        Ok(())
    }
}

// ---------- compose-aware diagnostics helper ----------

/// Convenience: dump `docker compose ps` and `logs --tail 200` to stderr on
/// failure. Called by the harness, exposed here so probes that want extra
/// noise on Err can also use it. Read-only; never returns Err.
pub fn dump_diagnostics(compose_file: &PathBuf, project: &str) {
    eprintln!("[e2e] --- diagnostics: docker compose ps -----------");
    let _ = Command::new("docker")
        .args(["compose", "-f"])
        .arg(compose_file)
        .args(["-p", project, "ps"])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();
    eprintln!("[e2e] --- diagnostics: docker compose logs --tail 200 ---");
    let mut child = match Command::new("docker")
        .args(["compose", "-f"])
        .arg(compose_file)
        .args(["-p", project, "logs", "--tail", "200"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[e2e] failed to spawn docker compose logs: {}", e);
            return;
        }
    };
    if let Some(mut out) = child.stdout.take() {
        let mut buf = String::new();
        let _ = out.read_to_string(&mut buf);
        eprintln!("{}", buf);
    }
    let _ = child.wait();
}

// ---------- per-stack probe matrix ----------

/// Return the readiness probe chain for `stack`. Probes are run in order, each
/// retried until success-or-deadline. Stacks consult `ctx.host_ports` to pick
/// the right published port; HTTP paths/credentials are baked here because
/// they are runtime-type-specific, not user-facing config.
///
/// Phase 4 environment-variable contract (chain selection):
///
/// | `FIRESTREAM_E2E_HEALTHD` | `FIRESTREAM_E2E_HTTP` | Chain                                                                            |
/// |--------------------------|-----------------------|----------------------------------------------------------------------------------|
/// | unset / `1` (default)    | unset / `1` (default) | `Tcp(<app_port>) → HealthEndpoint(/readyz + /sbom)` — preferred, single contract |
/// | unset / `1`              | `0`                   | `Tcp(<app_port>)` only (no HTTP at all; degenerate but documented)               |
/// | `0`                      | unset / `1`           | Per-protocol Phase-1 chain (PgIsReady, RedisPing, KafkaApiVersions, …)           |
/// | `0`                      | `0`                   | Per-protocol Phase-1 chain WITHOUT its HTTP component (SparkWithWorkers, …)      |
pub fn for_stack(stack: &str, ctx: &ProbeCtx) -> Vec<Box<dyn Probe_>> {
    if stack == "supabase" {
        return supabase_chain(ctx);
    }

    let http_enabled = std::env::var("FIRESTREAM_E2E_HTTP")
        .map(|v| v != "0")
        .unwrap_or(true);
    let healthd_enabled = std::env::var("FIRESTREAM_E2E_HEALTHD")
        .map(|v| v != "0")
        .unwrap_or(true);

    if healthd_enabled && http_enabled {
        let app_port = app_host_port(stack, ctx);
        let mut chain: Vec<Box<dyn Probe_>> = Vec::with_capacity(2);
        if let Some(p) = app_port {
            chain.push(boxed(Tcp { port: p }));
        }
        if let Some(hp) = ctx.health_host_port {
            chain.push(Box::new(HealthEndpoint {
                base_url: format!("http://127.0.0.1:{}", hp),
            }));
            return chain;
        }
        eprintln!(
            "[e2e:{}] WARN: HealthEndpoint requested but health_host_port=None; \
             falling back to per-protocol chain",
            stack
        );
    }

    // Per-protocol Phase-1 chain (preserved verbatim — this is the
    // truth-table for what each runtimeType actually needs to check).
    let exec = ctx.exec();
    match stack {
        "postgresql" => {
            let pg_port = app_host_port(stack, ctx).unwrap_or(5432);
            let mut chain: Vec<Box<dyn Probe_>> = vec![boxed(Tcp { port: pg_port })];
            if http_enabled {
                chain.push(boxed(PgIsReady {
                    exec: exec.clone(),
                    target: "postgresql".into(),
                    user: "postgres".into(),
                    db: "postgres".into(),
                }));
            }
            chain
        }
        "redis" => {
            let redis_port = app_host_port(stack, ctx).unwrap_or(6379);
            vec![
                boxed(Tcp { port: redis_port }),
                boxed(RedisPing {
                    exec: exec.clone(),
                    target: "redis".into(),
                }),
            ]
        }
        "kafka" => {
            let port = app_host_port(stack, ctx).unwrap_or(9092);
            vec![
                boxed(Tcp { port }),
                boxed(KafkaApiVersions {
                    exec: exec.clone(),
                    target: "kafka".into(),
                    bootstrap: "localhost:9092".into(),
                }),
            ]
        }
        "spark" => {
            let master_port = app_host_port(stack, ctx).unwrap_or(8080);
            let master_url = format!("http://127.0.0.1:{}", master_port);
            let mut chain: Vec<Box<dyn Probe_>> = vec![boxed(Tcp { port: master_port })];
            if http_enabled {
                chain.push(Box::new(SparkWithWorkers {
                    master_url,
                    min_workers: 0,
                }));
            }
            chain
        }
        "airflow" => {
            let api_port = app_host_port(stack, ctx).unwrap_or(8080);
            let mut chain: Vec<Box<dyn Probe_>> = vec![
                boxed(PgIsReady {
                    exec: exec.clone(),
                    target: "postgresql".into(),
                    user: "airflow".into(),
                    db: "airflow".into(),
                }),
                boxed(RedisPing {
                    exec: exec.clone(),
                    target: "redis".into(),
                }),
            ];
            if http_enabled {
                chain.push(boxed(HttpReady {
                    url: format!("http://127.0.0.1:{}/", api_port),
                    max_status: 500,
                }));
            }
            chain
        }
        "jupyterhub" => {
            let port = app_host_port(stack, ctx).unwrap_or(8000);
            let mut chain: Vec<Box<dyn Probe_>> = vec![boxed(Tcp { port })];
            if http_enabled {
                chain.push(boxed(HttpReady {
                    url: format!("http://127.0.0.1:{}/hub/", port),
                    max_status: 500,
                }));
            }
            chain
        }
        "superset" => {
            let port = app_host_port(stack, ctx).unwrap_or(8088);
            let mut chain: Vec<Box<dyn Probe_>> = vec![boxed(Tcp { port })];
            if http_enabled {
                chain.push(boxed(HttpReady {
                    url: format!("http://127.0.0.1:{}/health", port),
                    max_status: 500,
                }));
            }
            chain
        }
        "odoo" => {
            let port = app_host_port(stack, ctx).unwrap_or(8069);
            let mut chain: Vec<Box<dyn Probe_>> = vec![boxed(Tcp { port })];
            if http_enabled {
                chain.push(boxed(HttpReady {
                    url: format!("http://127.0.0.1:{}/web/login", port),
                    max_status: 500,
                }));
            }
            chain
        }
        other => {
            eprintln!(
                "[e2e:{}] WARN: no probe matrix defined; falling back to TCP-only",
                other
            );
            ctx.host_ports
                .iter()
                .copied()
                .map(|p| boxed(Tcp { port: p }))
                .collect()
        }
    }
}

/// The compose service and container port of a stack's main endpoint.
fn app_endpoint(stack: &str) -> Option<(&'static str, u16)> {
    match stack {
        "postgresql" => Some(("postgresql", 5432)),
        "redis" => Some(("redis", 6379)),
        "kafka" => Some(("kafka", 9092)),
        "spark" => Some(("spark", 8080)),
        "airflow" => Some(("airflow", 8080)),
        "jupyterhub" => Some(("jupyterhub", 8000)),
        "superset" => Some(("superset", 8088)),
        "odoo" => Some(("odoo", 8069)),
        _ => None,
    }
}

/// The host port of a stack's main endpoint, or its first published port
/// when the stack has no known endpoint.
fn app_host_port(stack: &str, ctx: &ProbeCtx) -> Option<u16> {
    app_endpoint(stack)
        .and_then(|(service, container)| ctx.host_port(service, container))
        .or_else(|| ctx.host_ports.first().copied())
}
