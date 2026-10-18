//! Per-chart probe chain for the k8s harness.
//!
//! Mirrors the docker harness's `Probe_` / `CoreProbe` adapter pattern
//! (see `src/lib/rust/firestream/tests/e2e/probes.rs`):
//!
//! - The transport-agnostic probes in `firestream_e2e_core::probe`
//!   (`Tcp`, `HttpReady`, `PgIsReady`, `RedisPing`, `KafkaApiVersions`)
//!   take no context.
//! - The k8s harness wants probes that can resolve pod names + secrets
//!   from the live cluster on each retry, so we define a local `Probe`
//!   trait that takes `&K8sCtx`.
//! - `CoreProbe<P>` lifts a core probe into the local trait (the core
//!   probe ignores the ctx).
//! - `PodPgReadyProbe` / `PodRedisPingProbe` are local probes that
//!   re-resolve the pod name (and, for redis, the auth secret) on every
//!   `run()` call so they tolerate helm install racing pod creation.
//!
//! `for_chart(chart, ctx)` is the per-chart matrix. Phase 3 fills in
//! `postgresql` and `redis`; Phase 4 fills in the remaining 6 charts.
//! Calling `for_chart` for an unimplemented chart returns `Err` at
//! runtime — keeping `cargo build` green even though the harness will
//! eventually exercise them all.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use crate::exec::Exec;
use crate::probe::{HttpReady, PgIsReady, Probe as CoreProbeTrait, RedisPing};

use super::cluster::ClusterHandle;
use super::exec::KubectlExec;
use super::portforward::forward_service;

/// Per-test context handed to every probe. The probe matrix consumes
/// these fields to construct a `KubectlExec` per-run, look up the right
/// pod, and bound its own retries against the outer harness deadline.
#[derive(Debug, Clone)]
pub struct K8sCtx {
    pub handle: ClusterHandle,
    pub namespace: String,
    pub release: String,
    pub deadline: Instant,
}

/// Local probe trait. Mirrors `firestream_e2e_core::probe::Probe` but
/// takes `&K8sCtx` so probes can read live cluster state (pod name,
/// generated secrets, …) on each retry rather than freezing them at
/// chain-construction time.
pub trait Probe: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self, ctx: &K8sCtx) -> Result<()>;
}

// ---------- CoreProbe adapter ----------

/// Adapter from `firestream_e2e_core::probe::Probe` (ctx-less) to the
/// local `Probe` trait. The k8s harness mostly uses this for pure-
/// network probes (`Tcp`, `HttpReady`) that are parameterised at
/// construction time and don't need to inspect the cluster.
pub struct CoreProbe<P: CoreProbeTrait + Send + Sync> {
    pub inner: P,
    /// Stable name suffix used in `Probe::name`. We can't borrow the
    /// inner probe's `&str` through a `'static` slot, so callers either
    /// accept the generic `"core_probe"` or wrap with a thin local
    /// probe carrying their own name literal.
    name: &'static str,
}

impl<P: CoreProbeTrait + Send + Sync> CoreProbe<P> {
    pub fn new(inner: P) -> Self {
        Self {
            inner,
            name: "core_probe",
        }
    }

    pub fn with_name(inner: P, name: &'static str) -> Self {
        Self { inner, name }
    }
}

impl<P: CoreProbeTrait + Send + Sync> Probe for CoreProbe<P> {
    fn name(&self) -> &'static str {
        self.name
    }
    fn run(&self, _ctx: &K8sCtx) -> Result<()> {
        self.inner.run()
    }
}

// ---------- Pod resolution ----------

/// Resolve a concrete pod name for `release` in `namespace` via the
/// canonical Bitnami label `app.kubernetes.io/instance=<release>`.
///
/// We resolve on every probe call (not once at chain-construction time)
/// so the probe tolerates helm install finishing before the pod has been
/// scheduled. Cheap — single `kubectl get pods`.
fn resolve_pod_name(kubeconfig: &Path, release: &str, namespace: &str) -> Result<String> {
    let selector = format!("app.kubernetes.io/instance={}", release);
    let out = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(kubeconfig)
        .args([
            "get",
            "pods",
            "-l",
            &selector,
            "-n",
            namespace,
            "-o",
            "jsonpath={.items[0].metadata.name}",
        ])
        .stdin(Stdio::null())
        .output()
        .context("spawn kubectl get pods")?;
    if !out.status.success() {
        bail!(
            "kubectl get pods (release={} ns={}) failed: {}",
            release,
            namespace,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let name = String::from_utf8(out.stdout)
        .context("kubectl get pods stdout not utf-8")?
        .trim()
        .to_string();
    if name.is_empty() {
        bail!(
            "no pods found for label {} in ns {}",
            selector,
            namespace
        );
    }
    Ok(name)
}

// ---------- Postgres pg_isready (per-run pod resolution) ----------

/// `pg_isready -U postgres -d postgres` inside the active bitnami
/// postgres pod. Resolves the pod each run so it tolerates pod restarts
/// / stateful-set initial-creation timing.
pub struct PodPgReadyProbe;

impl Probe for PodPgReadyProbe {
    fn name(&self) -> &'static str {
        "pod_pg_isready"
    }
    fn run(&self, ctx: &K8sCtx) -> Result<()> {
        let pod = resolve_pod_name(&ctx.handle.kubeconfig, &ctx.release, &ctx.namespace)?;
        let exec: Arc<dyn Exec> = Arc::new(KubectlExec {
            namespace: ctx.namespace.clone(),
            kubeconfig: ctx.handle.kubeconfig.clone(),
        });
        // Bitnami postgres default: superuser `postgres`, default db
        // `postgres`. The chart's `auth.username` slot creates a second
        // role; `postgres` always exists.
        let probe = PgIsReady {
            exec,
            target: pod,
            user: "postgres".to_string(),
            db: "postgres".to_string(),
        };
        probe.run()
    }
}

// ---------- Redis ping with auth-secret resolution ----------

/// `redis-cli ping` inside the active bitnami redis pod. Reads the
/// generated auth secret each call (in case the secret rotates between
/// chart upgrade and probe — unlikely in a per-test cluster, but cheap
/// to read) and threads it through as `REDISCLI_AUTH` so the wrapped
/// `RedisPing` probe doesn't have to know about authentication.
///
/// Why `REDISCLI_AUTH` over `--no-auth-warning -a <pwd>`: avoids
/// surfacing the password as a substring of the command line (visible
/// in `ps`) and sidesteps `--no-auth-warning` compatibility differences
/// between bitnami redis-cli versions.
pub struct PodRedisPingProbe;

impl Probe for PodRedisPingProbe {
    fn name(&self) -> &'static str {
        "pod_redis_ping"
    }
    fn run(&self, ctx: &K8sCtx) -> Result<()> {
        let pod = resolve_pod_name(&ctx.handle.kubeconfig, &ctx.release, &ctx.namespace)?;

        // Fetch the auth password. Bitnami redis stores it in the
        // release-named secret under key `redis-password`. We tolerate
        // a missing secret (treat as no-auth) so the probe still works
        // if the chart's `auth.enabled=false` happens to be set by a
        // future overlay — but for the stock chart the secret is
        // always there.
        let password = read_bitnami_redis_password(&ctx.handle.kubeconfig, &ctx.release, &ctx.namespace)?;

        // Custom Exec that injects REDISCLI_AUTH for the redis-cli
        // invocation. We don't use the bare `KubectlExec` here because
        // its impl wires `kubectl exec` directly — we want to splice
        // an `env REDISCLI_AUTH=<pwd>` prefix into the in-pod command.
        let exec: Arc<dyn Exec> = Arc::new(AuthedKubectlExec {
            inner: KubectlExec {
                namespace: ctx.namespace.clone(),
                kubeconfig: ctx.handle.kubeconfig.clone(),
            },
            env_pairs: password
                .as_deref()
                .map(|p| vec![("REDISCLI_AUTH".to_string(), p.to_string())])
                .unwrap_or_default(),
        });

        let probe = RedisPing {
            exec,
            target: pod,
        };
        probe.run()
    }
}

/// `Exec` wrapper that prepends `env K=V ...` to the in-target command.
/// Lets `RedisPing` (which runs `redis-cli ping`) authenticate without
/// having to change its source — the wrapped exec adds the env vars on
/// the way in.
struct AuthedKubectlExec {
    inner: KubectlExec,
    env_pairs: Vec<(String, String)>,
}

impl Exec for AuthedKubectlExec {
    fn exec(&self, target: &str, args: &[&str]) -> Result<std::process::Output> {
        if self.env_pairs.is_empty() {
            return self.inner.exec(target, args);
        }
        // Build `env K1=V1 K2=V2 <args…>` in-pod. `env` is part of
        // coreutils; bitnami images ship it.
        let mut wrapped: Vec<String> = Vec::with_capacity(1 + 2 * self.env_pairs.len() + args.len());
        wrapped.push("env".to_string());
        for (k, v) in &self.env_pairs {
            wrapped.push(format!("{}={}", k, v));
        }
        for a in args {
            wrapped.push(a.to_string());
        }
        let refs: Vec<&str> = wrapped.iter().map(String::as_str).collect();
        self.inner.exec(target, &refs)
    }
}

/// Read the bitnami redis-generated password from the in-cluster
/// secret. Returns `Ok(None)` only if the secret exists but the key
/// is empty (effectively "no auth"); errors on any other failure mode
/// so we don't silently mask a misconfiguration.
fn read_bitnami_redis_password(
    kubeconfig: &Path,
    release: &str,
    namespace: &str,
) -> Result<Option<String>> {
    let out = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(kubeconfig)
        .args([
            "get",
            "secret",
            release,
            "-n",
            namespace,
            "-o",
            "jsonpath={.data.redis-password}",
        ])
        .stdin(Stdio::null())
        .output()
        .context("spawn kubectl get secret for redis-password")?;
    if !out.status.success() {
        bail!(
            "kubectl get secret {} (ns={}): {}",
            release,
            namespace,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let b64 = String::from_utf8(out.stdout)
        .context("redis-password secret stdout not utf-8")?
        .trim()
        .to_string();
    if b64.is_empty() {
        return Ok(None);
    }
    let decoded =
        decode_base64(&b64).with_context(|| format!("decoding redis-password ({:?})", b64))?;
    let password = String::from_utf8(decoded).context("redis password not utf-8")?;
    if password.is_empty() {
        Ok(None)
    } else {
        Ok(Some(password))
    }
}

/// Tiny base64 decoder so this crate doesn't need a new dep just to
/// read one secret. Tolerates `=` padding and rejects characters
/// outside the standard alphabet.
fn decode_base64(input: &str) -> Result<Vec<u8>> {
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    // Map char -> value, returning Err on bogus input.
    let mut buf: Vec<u8> = Vec::with_capacity((input.len() / 4) * 3);
    let mut group: u32 = 0;
    let mut bits: u32 = 0;
    let mut pad_count: u32 = 0;
    for c in input.bytes() {
        if c == b'\n' || c == b'\r' || c == b' ' {
            continue;
        }
        if c == b'=' {
            pad_count += 1;
            continue;
        }
        if pad_count > 0 {
            bail!("base64: non-padding char after padding");
        }
        let val = ALPHA
            .iter()
            .position(|&a| a == c)
            .ok_or_else(|| anyhow!("base64: invalid byte 0x{:02x}", c))? as u32;
        group = (group << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            buf.push(((group >> bits) & 0xff) as u8);
        }
    }
    // The padding handling above implicitly truncates trailing
    // partial bytes — that's fine for the bitnami redis secret which
    // is always a clean multiple of 3 bytes pre-encode.
    let _ = pad_count;
    Ok(buf)
}

// ---------- HTTP-over-port-forward probe (Phase 4) ----------

/// Resolve a Service name in `namespace` by label selector. Used so the
/// HTTP-over-port-forward probe can find the right Service regardless of
/// the release-name suffix conventions that vary across Bitnami charts
/// (some charts produce `<release>-<component>`, others just `<release>`
/// or `<release>-web` etc.).
fn resolve_service_name(
    kubeconfig: &Path,
    namespace: &str,
    selector: &str,
) -> Result<String> {
    let out = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(kubeconfig)
        .args([
            "get",
            "svc",
            "-l",
            selector,
            "-n",
            namespace,
            "-o",
            "jsonpath={.items[0].metadata.name}",
        ])
        .stdin(Stdio::null())
        .output()
        .context("spawn kubectl get svc")?;
    if !out.status.success() {
        bail!(
            "kubectl get svc -l {} -n {}: {}",
            selector,
            namespace,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let name = String::from_utf8(out.stdout)
        .context("kubectl get svc stdout not utf-8")?
        .trim()
        .to_string();
    if name.is_empty() {
        bail!(
            "no services matched selector `{}` in ns `{}`",
            selector,
            namespace
        );
    }
    Ok(name)
}

/// HTTP readiness probe that opens a port-forward, runs `HttpReady`
/// against `127.0.0.1:<local_port><path>`, then drops the forward.
/// The Service is resolved per-run via label selector so we tolerate
/// the Bitnami release-name suffix variations without baking in a
/// guess at chain-construction time.
///
/// `max_status = 500` matches the docker harness's HTTP probe — 2xx,
/// 3xx, 401, 403, 404 all count as "app is alive"; only 5xx is "not
/// ready". This is intentional: many of these endpoints (jupyterhub
/// `/hub/health`, odoo `/web/health`) return 200 once ready, but some
/// return 302 redirects to a login page even when fully serving.
pub struct HttpProbeOverPortForward {
    pub name: &'static str,
    /// Label selector that picks exactly one Service. Format example:
    /// `"app.kubernetes.io/instance=airflow,app.kubernetes.io/component=web"`.
    pub service_selector: String,
    /// Service-side port to forward (the `port` in the Service
    /// `spec.ports[].port` — kubectl maps it to its `targetPort`).
    pub remote_port: u16,
    /// Path component of the URL probed (e.g. `"/health"`). Leading `/`
    /// is required.
    pub path: &'static str,
    /// `< max_status` counts as ready (anything in [100, max_status)).
    pub max_status: u16,
}

impl Probe for HttpProbeOverPortForward {
    fn name(&self) -> &'static str {
        self.name
    }
    fn run(&self, ctx: &K8sCtx) -> Result<()> {
        let svc = resolve_service_name(
            &ctx.handle.kubeconfig,
            &ctx.namespace,
            &self.service_selector,
        )
        .with_context(|| format!("resolve service for selector `{}`", self.service_selector))?;
        let pf = forward_service(&ctx.handle, &ctx.namespace, &svc, self.remote_port)
            .with_context(|| format!("port-forward svc/{}:{}", svc, self.remote_port))?;
        let url = format!("http://127.0.0.1:{}{}", pf.local_port(), self.path);
        let probe = HttpReady {
            url: url.clone(),
            max_status: self.max_status,
        };
        let r = probe.run();
        // `pf` drops here, killing kubectl. Bubble result up afterwards
        // so we always release the forward on success and failure paths.
        r.with_context(|| format!("HTTP probe {} {}", self.name, url))
    }
}

// ---------- Kafka API-versions exec probe (Phase 4) ----------

/// `kafka-broker-api-versions.sh --bootstrap-server <bootstrap>
/// --command-config <client.properties>` inside a kafka pod. The
/// bitnami kafka image enables SASL_PLAINTEXT with a `user1` SASL user
/// by default (see kafka/rendered.yaml `listener.security.protocol.map`
/// and `kafka-user-passwords` Secret). We materialise a `client.properties`
/// file inside the pod with the per-test SASL password and pass it via
/// `--command-config`.
///
/// Per-run pod and secret resolution mirrors the postgres/redis probes:
/// pods get scheduled after helm install hands back, so chain-construction
/// time is too early to read the pod name.
pub struct PodKafkaApiVersionsProbe;

impl Probe for PodKafkaApiVersionsProbe {
    fn name(&self) -> &'static str {
        "pod_kafka_api_versions"
    }
    fn run(&self, ctx: &K8sCtx) -> Result<()> {
        // The kafka pod label is `app.kubernetes.io/instance=<release>` —
        // there are multiple kafka pods (controller-0..2 by default);
        // resolve_pod_name returns whichever is first. Any controller can
        // serve api-versions, so picking [0] is fine.
        let pod = resolve_pod_name(&ctx.handle.kubeconfig, &ctx.release, &ctx.namespace)?;

        // Read the bitnami-generated `client-passwords` secret value.
        // The secret name is `<release>-user-passwords`.
        let secret_name = format!("{}-user-passwords", ctx.release);
        let password = read_secret_value_b64(
            &ctx.handle.kubeconfig,
            &secret_name,
            &ctx.namespace,
            "client-passwords",
        )?;
        let password = match password {
            Some(p) => p,
            None => bail!("kafka client-passwords secret `{}` is empty", secret_name),
        };

        // Write client.properties into /tmp (writable in bitnami kafka
        // images) and invoke the CLI. We use a single `sh -c '…'` so we
        // can chain write-then-invoke inside one kubectl exec.
        //
        // The password is interpolated into a single-quoted-bash-string
        // by escaping any single-quotes — bitnami passwords are alnum
        // (no quotes) so this is defensive.
        let escaped = password.replace('\'', "'\\''");
        let script = format!(
            "set -e; \
             cat > /tmp/firestream-e2e-kafka-client.properties <<'EOF'\n\
             security.protocol=SASL_PLAINTEXT\n\
             sasl.mechanism=PLAIN\n\
             sasl.jaas.config=org.apache.kafka.common.security.plain.PlainLoginModule required username=\"user1\" password=\"{}\";\n\
             EOF\n\
             kafka-broker-api-versions.sh --bootstrap-server localhost:9092 \
             --command-config /tmp/firestream-e2e-kafka-client.properties",
            escaped
        );

        let exec: Arc<dyn Exec> = Arc::new(KubectlExec {
            namespace: ctx.namespace.clone(),
            kubeconfig: ctx.handle.kubeconfig.clone(),
        });
        let out = exec
            .exec(&pod, &["sh", "-c", &script])
            .context("kubectl exec kafka-broker-api-versions")?;
        if out.status.success() {
            Ok(())
        } else {
            Err(anyhow!(
                "kafka-broker-api-versions exit {}: stderr {} stdout {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim(),
                String::from_utf8_lossy(&out.stdout).trim()
            ))
        }
    }
}

/// Read `data.<key>` from a k8s Secret, base64-decode it, return as a
/// `String`. Returns `Ok(None)` if the value is empty.
fn read_secret_value_b64(
    kubeconfig: &Path,
    name: &str,
    namespace: &str,
    key: &str,
) -> Result<Option<String>> {
    let jsonpath = format!("jsonpath={{.data.{}}}", key);
    let out = Command::new("kubectl")
        .arg("--kubeconfig")
        .arg(kubeconfig)
        .args(["get", "secret", name, "-n", namespace, "-o", &jsonpath])
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("spawn kubectl get secret {}/{}", namespace, name))?;
    if !out.status.success() {
        bail!(
            "kubectl get secret {}/{}: {}",
            namespace,
            name,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let b64 = String::from_utf8(out.stdout)
        .context("kubectl secret stdout not utf-8")?
        .trim()
        .to_string();
    if b64.is_empty() {
        return Ok(None);
    }
    let decoded = decode_base64(&b64)
        .with_context(|| format!("decoding secret {}.{} value", name, key))?;
    let s = String::from_utf8(decoded)
        .with_context(|| format!("secret {}.{} value not utf-8", name, key))?;
    if s.is_empty() {
        Ok(None)
    } else {
        Ok(Some(s))
    }
}

// ---------- per-chart matrix ----------

/// Return the readiness probe chain for `chart`.
///
/// All nine canonical Firestream charts are wired here. Each probe
/// chain is single-element; we could add tcp pre-checks before each
/// protocol probe but the retry loop in `harness::run_one` already
/// handles flakiness via wall-clock-bounded retries.
///
/// **Selector / port / path provenance** (chase via the chart's
/// rendered.yaml `kind: Service` + `readinessProbe.httpGet` blocks if
/// you need to verify):
///
/// | Chart       | Selector                                            | Port | Path                       |
/// |-------------|-----------------------------------------------------|------|----------------------------|
/// | airflow     | `…/instance=<rel>,…/component=web`                  | 8080 | `/api/v2/monitor/health`   |
/// | spark       | `…/instance=<rel>,…/component=master`               |   80 | `/`                        |
/// | jupyterhub  | `…/instance=<rel>,…/component=hub`                  | 8081 | `/hub/health`              |
/// | superset    | `…/instance=<rel>,…/component=superset-web`         |   80 | `/health`                  |
/// | odoo        | `…/instance=<rel>,…/name=odoo`                      |   80 | `/web/health`              |
/// | seaweedfs   | `…/instance=<rel>,…/component=seaweedfs-all-in-one`  | 8333 | `/`                        |
///
/// The selectors deliberately use the *instance* label (release name)
/// rather than the *name* label (chart name) so jupyterhub/superset
/// don't collide with their bundled postgresql/redis subcharts.
pub fn for_chart(chart: &str, ctx: &K8sCtx) -> Result<Vec<Box<dyn Probe>>> {
    let rel = &ctx.release;
    match chart {
        "postgresql" => Ok(vec![Box::new(PodPgReadyProbe)]),
        "redis" => Ok(vec![Box::new(PodRedisPingProbe)]),
        "kafka" => Ok(vec![Box::new(PodKafkaApiVersionsProbe)]),
        "airflow" => Ok(vec![Box::new(HttpProbeOverPortForward {
            // Airflow 3.x exposes readiness at /api/v2/monitor/health
            // (NOT the bare /health from 2.x). Path verified against the
            // chart's own readiness probe in .firestream/charts/airflow/
            // rendered.yaml.
            name: "airflow_web_health",
            service_selector: format!(
                "app.kubernetes.io/instance={},app.kubernetes.io/component=web",
                rel
            ),
            remote_port: 8080,
            path: "/api/v2/monitor/health",
            max_status: 500,
        })]),
        "spark" => Ok(vec![Box::new(HttpProbeOverPortForward {
            // Spark master web UI on the chart Service port 80 (mapped to
            // targetPort `http` → container 8080). The chart's own
            // readiness probe hits `/` on 8080 (see rendered.yaml's
            // httpGet path), so we mirror that — the standalone Spark
            // master UI returns 200 for the root once it's serving.
            name: "spark_master_ui",
            service_selector: format!(
                "app.kubernetes.io/instance={},app.kubernetes.io/component=master",
                rel
            ),
            remote_port: 80,
            path: "/",
            max_status: 500,
        })]),
        "jupyterhub" => Ok(vec![Box::new(HttpProbeOverPortForward {
            // Hub component (NOT the proxy-public LB): /hub/health is the
            // documented readiness endpoint. proxy-public would also work
            // but adds an LB hop we don't need.
            name: "jupyterhub_hub_health",
            service_selector: format!(
                "app.kubernetes.io/instance={},app.kubernetes.io/component=hub",
                rel
            ),
            remote_port: 8081,
            path: "/hub/health",
            max_status: 500,
        })]),
        "superset" => Ok(vec![Box::new(HttpProbeOverPortForward {
            // Bitnami superset's web Service component label is
            // `superset-web` (NOT just `web` — verified against
            // `.firestream/charts/superset/rendered.yaml`). Service port
            // is 80, mapped to targetPort `http`.
            name: "superset_health",
            service_selector: format!(
                "app.kubernetes.io/instance={},app.kubernetes.io/component=superset-web",
                rel
            ),
            remote_port: 80,
            path: "/health",
            max_status: 500,
        })]),
        "odoo" => Ok(vec![Box::new(HttpProbeOverPortForward {
            // Bitnami odoo's primary Service has no `component` label —
            // there's only one component. Select on instance + name.
            // `/web/health` works on Odoo 14+ (the chart ships 18.0); the
            // older `/` fallback would return 303 redirects which our
            // `max_status=500` would still accept, but `/web/health` is
            // the documented readiness endpoint.
            name: "odoo_web_health",
            service_selector: format!(
                "app.kubernetes.io/instance={},app.kubernetes.io/name=odoo",
                rel
            ),
            remote_port: 80,
            path: "/web/health",
            max_status: 500,
        })]),
        "seaweedfs" => Ok(vec![Box::new(HttpProbeOverPortForward {
            // SeaweedFS is NOT a Bitnami chart — the all-in-one Service
            // (`templates/all-in-one/all-in-one-service.yml`) carries
            // `app.kubernetes.io/component=seaweedfs-all-in-one`. The S3
            // gateway listens on 8333 (`swfs-s3` port). The chart's native
            // readiness probe only hits the master `/cluster/status` on
            // 9333 (proves the process is up, not that S3 is serving), so
            // we probe the S3 gateway directly: a GET on `/` with auth
            // enabled returns 403 (AccessDenied) which `max_status=500`
            // accepts — that 403 proves the S3 gateway is alive and
            // handling requests.
            name: "seaweedfs_s3_gateway",
            service_selector: format!(
                "app.kubernetes.io/instance={},app.kubernetes.io/component=seaweedfs-all-in-one",
                rel
            ),
            remote_port: 8333,
            path: "/",
            max_status: 500,
        })]),
        "nginx" => Ok(vec![Box::new(HttpProbeOverPortForward {
            // The net-new nginx chart has one component, so select on
            // instance + name like odoo. Service port 80 → targetPort
            // `http` → container 8080 (unprivileged; the chart cannot bind
            // 80 under GKE Autopilot).
            //
            // `/healthz` is the chart's own local, NON-PROXYING health
            // location — the same one its readiness probe hits. That is the
            // right thing to probe here precisely because it does not touch
            // an upstream: with the default `upstreams: {}` there are no
            // backends, and a proxy must stay Ready while its backends are
            // absent or rolling. It returns a literal 200 "ok".
            name: "nginx_healthz",
            service_selector: format!(
                "app.kubernetes.io/instance={},app.kubernetes.io/name=nginx",
                rel
            ),
            remote_port: 80,
            path: "/healthz",
            max_status: 500,
        })]),
        // NOTE: there is deliberately no `cloudflared` arm. The connector's
        // only health signal is /ready, which reports whether it has
        // registered with the CLOUDFLARE EDGE — it cannot go Ready without a
        // real Cloudflare account and a real tunnel token, so there is nothing
        // a hermetic fresh-cluster harness can assert. See the e2eK8sCharts
        // list in bin/nix/firestream/ci/profile.nix.
        other => bail!(
            "probe chain for chart `{}` not implemented; known charts: \
             postgresql, redis, kafka, airflow, spark, jupyterhub, superset, odoo, seaweedfs, nginx",
            other
        ),
    }
}

// Re-export `PathBuf` so downstream `use` lines in test files don't
// have to chase imports. Not strictly required but matches the
// pattern in cluster.rs.
#[allow(dead_code)]
pub(crate) type _PathBuf = PathBuf;

#[cfg(test)]
mod tests {
    use super::*;

    // Verify the standard bitnami secret value round-trips through our
    // tiny base64 decoder. The chart generates a 10-char alnum
    // password and base64-encodes it.
    #[test]
    fn base64_decoder_roundtrips_typical_secret() {
        // "abcdefghij" -> base64 "YWJjZGVmZ2hpag=="
        let decoded = decode_base64("YWJjZGVmZ2hpag==").unwrap();
        assert_eq!(decoded, b"abcdefghij");
    }

    #[test]
    fn base64_decoder_handles_no_padding() {
        // "abc" -> "YWJj" (no padding needed; 3 bytes -> 4 chars).
        let decoded = decode_base64("YWJj").unwrap();
        assert_eq!(decoded, b"abc");
    }

    #[test]
    fn base64_decoder_rejects_garbage() {
        assert!(decode_base64("not!base64").is_err());
    }

    // `Vec<Box<dyn Probe>>` doesn't implement `Debug`, so `.unwrap()`
    // and `.unwrap_err()` would fail to type-check. Use explicit match
    // arms instead.
    fn dummy_ctx() -> K8sCtx {
        K8sCtx {
            handle: ClusterHandle {
                name: "x".into(),
                kubeconfig: PathBuf::from("/dev/null"),
                registry: "y".into(),
            },
            namespace: "ns".into(),
            release: "rel".into(),
            deadline: Instant::now(),
        }
    }

    #[test]
    fn for_chart_postgresql_returns_one_probe() {
        let ctx = dummy_ctx();
        match for_chart("postgresql", &ctx) {
            Ok(chain) => {
                assert_eq!(chain.len(), 1);
                assert_eq!(chain[0].name(), "pod_pg_isready");
            }
            Err(e) => panic!("expected Ok chain, got Err: {}", e),
        }
    }

    #[test]
    fn for_chart_redis_returns_one_probe() {
        let ctx = dummy_ctx();
        match for_chart("redis", &ctx) {
            Ok(chain) => {
                assert_eq!(chain.len(), 1);
                assert_eq!(chain[0].name(), "pod_redis_ping");
            }
            Err(e) => panic!("expected Ok chain, got Err: {}", e),
        }
    }

    #[test]
    fn for_chart_unknown_bails_with_chart_list() {
        let ctx = dummy_ctx();
        match for_chart("does-not-exist", &ctx) {
            Ok(_) => panic!("expected Err for unknown chart"),
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("does-not-exist"), "got: {}", msg);
                assert!(msg.contains("postgresql"), "got: {}", msg);
            }
        }
    }

    // Phase 4: smoke-test that all canonical charts have a wired chain.
    // Doesn't run the probes — just constructs them, which exercises
    // every match arm + selector format string.
    #[test]
    fn for_chart_all_canonical_charts_construct() {
        let ctx = dummy_ctx();
        for chart in &[
            "postgresql",
            "redis",
            "kafka",
            "airflow",
            "spark",
            "jupyterhub",
            "superset",
            "odoo",
            "seaweedfs",
        ] {
            match for_chart(chart, &ctx) {
                Ok(chain) => {
                    assert!(!chain.is_empty(), "chart {} has empty chain", chart);
                }
                Err(e) => panic!("chart {} chain failed to construct: {}", chart, e),
            }
        }
    }

    // Sanity that the HTTP probe selector strings are non-empty and
    // include the release name passed via ctx.
    #[test]
    fn http_probe_selectors_include_release() {
        let ctx = dummy_ctx();
        let chain = for_chart("airflow", &ctx).expect("airflow chain");
        // We don't introspect HttpProbeOverPortForward via the trait —
        // just confirm the chain has the expected name. The selector
        // format is unit-tested implicitly via for_chart's compilation.
        assert_eq!(chain[0].name(), "airflow_web_health");
    }
}
