//! Find and remove resources that e2e harnesses left behind.
//!
//! The sweeper removes only objects that a harness owns:
//!
//! | Kind | Owned when |
//! |---|---|
//! | k3d cluster | name starts `firestream-e2e-` |
//! | docker container, network, volume (k3d) | name starts `k3d-firestream-e2e-` **and** label `app=k3d` |
//! | compose container, volume, network | label `com.docker.compose.project` starts `firestream-e2e-` |
//! | anonymous volume | mounted by an owned compose container, 64-hex name, no compose project label |
//! | kubeconfig file | `$TMPDIR/firestream-e2e-*.kubeconfig` |
//! | kubeconfig context / cluster | `k3d-firestream-e2e-*` in `$KUBECONFIG` or the default file |
//! | kubeconfig user | `admin@k3d-firestream-e2e-*` |
//!
//! Dev stacks (`firestream-<stack>`) and other projects never match, because
//! every filter needs the `firestream-e2e-` prefix (see [`Scope`]).
//!
//! # Flow
//!
//! [`discover`] lists what exists, [`plan`] orders the removals, and
//! [`execute`] runs them under the host lock. [`sweep`] does all three.
//!
//! k3d order: `k3d registry delete`, `k3d cluster delete`, `docker rm` of
//! leftover nodes, `docker network rm`, `docker volume rm`, kubeconfig file,
//! kubeconfig context. Compose order, per project: read `.Mounts` (during
//! discovery), `docker rm` containers, `docker volume rm` the anonymous
//! volumes from those mounts, `docker volume rm` named volumes,
//! `docker network rm`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::lock::HostLock;
use crate::run_id::{K3D_PREFIX, PREFIX, RunId, parse_resource_name};
use crate::runner::{CmdOutput, CommandRunner};

const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";

/// Which harness resources a sweep or leak check looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every `firestream-e2e-*` resource.
    All,
    /// Resources named `firestream-e2e-<any target>-<id>`.
    Run(RunId),
    /// One exact resource name, `firestream-e2e-<target>-<id>`.
    Name(String),
}

impl Scope {
    /// Scope for `target` in run `run`, or for the whole run if `target`
    /// is `None`.
    pub fn for_run(run: &RunId, target: Option<&str>) -> Scope {
        match target {
            Some(t) => Scope::Name(run.resource_name(t)),
            None => Scope::Run(run.clone()),
        }
    }

    /// Does a k3d cluster name or compose project name belong to this
    /// scope?
    pub fn matches_name(&self, name: &str) -> bool {
        match self {
            Scope::All => name.len() > PREFIX.len() && name.starts_with(PREFIX),
            Scope::Run(id) => parse_resource_name(name).is_some_and(|(_, got)| &got == id),
            Scope::Name(exact) => name == exact,
        }
    }

    /// Does a k3d-derived object name (`k3d-<cluster>`,
    /// `k3d-<cluster>-server-0`, `k3d-<cluster>-images`, …) belong to this
    /// scope? The label check is separate.
    pub fn matches_k3d_object(&self, object: &str) -> bool {
        if !object.starts_with(K3D_PREFIX) {
            return false;
        }
        let rest = &object["k3d-".len()..];
        cluster_candidates(rest).iter().any(|c| self.matches_name(c))
    }

    /// Short human-readable name of the scope for log lines.
    pub fn describe(&self) -> String {
        match self {
            Scope::All => "all firestream-e2e-* resources".to_string(),
            Scope::Run(id) => format!("run {id}"),
            Scope::Name(n) => format!("resource {n}"),
        }
    }
}

/// `rest` itself, plus `rest` without one known k3d node or object suffix.
fn cluster_candidates(rest: &str) -> Vec<&str> {
    let mut out = vec![rest];
    for suffix in ["-serverlb", "-tools", "-registry", "-images"] {
        if let Some(c) = rest.strip_suffix(suffix) {
            out.push(c);
        }
    }
    for role in ["-server-", "-agent-"] {
        if let Some((c, n)) = rest.rsplit_once(role)
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
        {
            out.push(c);
        }
    }
    out
}

/// One compose project and the objects that belong to it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ComposeProject {
    pub name: String,
    /// `(id, name)` of each container.
    pub containers: Vec<(String, String)>,
    /// Anonymous volumes mounted by those containers.
    pub anonymous_volumes: Vec<String>,
    pub named_volumes: Vec<String>,
    pub networks: Vec<String>,
}

/// Every harness-owned object found on the host for one [`Scope`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory {
    /// `k3d cluster list` worked. If not, k3d objects are removed with
    /// `docker` only.
    pub k3d_available: bool,
    pub k3d_clusters: Vec<String>,
    /// Registry container names (`k3d-<cluster>-registry`).
    pub k3d_registries: Vec<String>,
    /// Other `app=k3d` containers (server, serverlb, tools, agents).
    pub k3d_containers: Vec<String>,
    pub k3d_networks: Vec<String>,
    pub k3d_volumes: Vec<String>,
    pub kubeconfig_files: Vec<PathBuf>,
    pub kube_contexts: Vec<String>,
    pub kube_clusters: Vec<String>,
    pub kube_users: Vec<String>,
    pub compose: Vec<ComposeProject>,
    /// An optional tool (`k3d`, `kubectl`) is missing; its objects were not
    /// checked.
    pub warnings: Vec<String>,
    /// A required listing (`docker`, the temp dir) failed, so the
    /// inventory may be incomplete.
    pub errors: Vec<String>,
}

impl Inventory {
    /// One line per object: `"<kind> <name>"`.
    pub fn objects(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut push = |kind: &str, items: &[String]| {
            out.extend(items.iter().map(|i| format!("{kind} {i}")));
        };
        push("k3d cluster", &self.k3d_clusters);
        push("k3d registry", &self.k3d_registries);
        push("k3d container", &self.k3d_containers);
        push("k3d network", &self.k3d_networks);
        push("k3d volume", &self.k3d_volumes);
        let files: Vec<String> = self.kubeconfig_files.iter().map(|p| p.display().to_string()).collect();
        push("kubeconfig file", &files);
        push("kubeconfig context", &self.kube_contexts);
        push("kubeconfig cluster", &self.kube_clusters);
        push("kubeconfig user", &self.kube_users);
        for p in &self.compose {
            let containers: Vec<String> = p.containers.iter().map(|(_, n)| n.clone()).collect();
            push(&format!("compose[{}] container", p.name), &containers);
            push(&format!("compose[{}] anonymous volume", p.name), &p.anonymous_volumes);
            push(&format!("compose[{}] volume", p.name), &p.named_volumes);
            push(&format!("compose[{}] network", p.name), &p.networks);
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.objects().is_empty()
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// Run a listing command. `Err` holds a one-line reason.
fn list(runner: &dyn CommandRunner, program: &str, a: &[&str]) -> Result<String, String> {
    let full = args(a);
    match runner.run(program, &full) {
        Ok(out) if out.success() => Ok(out.stdout),
        Ok(out) => Err(format!(
            "`{program} {}` failed: {}",
            a.join(" "),
            first_line(&out.stderr)
        )),
        Err(e) => Err(format!("`{program} {}` could not start: {e}", a.join(" "))),
    }
}

fn first_line(s: &str) -> &str {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim()
}

/// Split a `--format` line on tabs, padding missing fields with "".
fn fields(line: &str, n: usize) -> Vec<&str> {
    let mut f: Vec<&str> = line.split('\t').map(str::trim).collect();
    f.resize(n, "");
    f
}

fn is_anonymous_volume_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// List every harness-owned object in `scope`. `tmp_dir` is where the k3d
/// harness writes kubeconfig files (`std::env::temp_dir()`).
pub fn discover(runner: &dyn CommandRunner, scope: &Scope, tmp_dir: &Path) -> Inventory {
    let mut inv = Inventory::default();
    discover_k3d(runner, scope, &mut inv);
    discover_kubeconfig_files(scope, tmp_dir, &mut inv);
    discover_kube_entries(runner, scope, &mut inv);
    discover_compose(runner, scope, &mut inv);
    inv
}

fn discover_k3d(runner: &dyn CommandRunner, scope: &Scope, inv: &mut Inventory) {
    match list(runner, "k3d", &["cluster", "list", "-o", "json"]) {
        Ok(json) => match serde_json::from_str::<serde_json::Value>(&json) {
            Ok(v) => {
                inv.k3d_available = true;
                let names = v.as_array().into_iter().flatten().filter_map(|c| c.get("name")?.as_str());
                inv.k3d_clusters = names.filter(|n| scope.matches_name(n)).map(String::from).collect();
            }
            Err(e) => inv.warnings.push(format!("cannot parse `k3d cluster list -o json`: {e}")),
        },
        Err(e) => inv
            .warnings
            .push(format!("{e}; k3d clusters are removed with docker only")),
    }

    let fmt = "{{.Names}}\t{{.Label \"app\"}}\t{{.Label \"k3d.role\"}}";
    match list(runner, "docker", &["ps", "-a", "--filter", "label=app=k3d", "--format", fmt]) {
        Ok(out) => {
            for line in out.lines().filter(|l| !l.trim().is_empty()) {
                let f = fields(line, 3);
                if f[1] != "k3d" || !scope.matches_k3d_object(f[0]) {
                    continue;
                }
                if f[2] == "registry" {
                    inv.k3d_registries.push(f[0].to_string());
                } else {
                    inv.k3d_containers.push(f[0].to_string());
                }
            }
        }
        Err(e) => inv.errors.push(e),
    }

    for (kind, target) in [("network", &mut inv.k3d_networks), ("volume", &mut inv.k3d_volumes)] {
        let fmt = "{{.Name}}\t{{.Label \"app\"}}";
        match list(runner, "docker", &[kind, "ls", "--filter", "label=app=k3d", "--format", fmt]) {
            Ok(out) => {
                for line in out.lines().filter(|l| !l.trim().is_empty()) {
                    let f = fields(line, 2);
                    if f[1] == "k3d" && scope.matches_k3d_object(f[0]) {
                        target.push(f[0].to_string());
                    }
                }
            }
            Err(e) => inv.errors.push(e),
        }
    }
}

fn discover_kubeconfig_files(scope: &Scope, tmp_dir: &Path, inv: &mut Inventory) {
    let entries = match std::fs::read_dir(tmp_dir) {
        Ok(e) => e,
        Err(e) => {
            inv.errors.push(format!("cannot read {}: {e}", tmp_dir.display()));
            return;
        }
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.strip_suffix(".kubeconfig").is_some_and(|c| scope.matches_name(c))
        })
        .map(|e| e.path())
        .collect();
    files.sort();
    inv.kubeconfig_files = files;
}

fn discover_kube_entries(runner: &dyn CommandRunner, scope: &Scope, inv: &mut Inventory) {
    let matches_k3d_name = |name: &str| name.strip_prefix("k3d-").is_some_and(|c| scope.matches_name(c));
    type ClusterPart = fn(&str) -> &str;
    let lookups: [(&str, &mut Vec<String>, ClusterPart); 3] = [
        ("get-contexts", &mut inv.kube_contexts, |n| n),
        ("get-clusters", &mut inv.kube_clusters, |n| n),
        ("get-users", &mut inv.kube_users, |n| n.strip_prefix("admin@").unwrap_or("")),
    ];
    for (verb, target, cluster_part) in lookups {
        let a: &[&str] = if verb == "get-contexts" {
            &["config", verb, "-o", "name"]
        } else {
            &["config", verb]
        };
        match list(runner, "kubectl", a) {
            Ok(out) => {
                for name in out.lines().map(str::trim).filter(|l| !l.is_empty() && *l != "NAME") {
                    if matches_k3d_name(cluster_part(name)) {
                        target.push(name.to_string());
                    }
                }
            }
            Err(e) => {
                inv.warnings.push(format!("{e}; kubeconfig entries not checked"));
                return;
            }
        }
    }
}

fn discover_compose(runner: &dyn CommandRunner, scope: &Scope, inv: &mut Inventory) {
    let project_filter = format!("label={COMPOSE_PROJECT_LABEL}");
    let mut projects: BTreeMap<String, ComposeProject> = BTreeMap::new();
    fn project<'a>(
        projects: &'a mut BTreeMap<String, ComposeProject>,
        scope: &Scope,
        name: &str,
    ) -> Option<&'a mut ComposeProject> {
        if !scope.matches_name(name) {
            return None;
        }
        Some(projects.entry(name.to_string()).or_insert_with(|| ComposeProject {
            name: name.to_string(),
            ..Default::default()
        }))
    }

    let fmt = format!("{{{{.ID}}}}\t{{{{.Names}}}}\t{{{{.Label \"{COMPOSE_PROJECT_LABEL}\"}}}}");
    match list(runner, "docker", &["ps", "-a", "--filter", &project_filter, "--format", &fmt]) {
        Ok(out) => {
            for line in out.lines().filter(|l| !l.trim().is_empty()) {
                let f = fields(line, 3);
                if let Some(p) = project(&mut projects, scope, f[2]) {
                    p.containers.push((f[0].to_string(), f[1].to_string()));
                }
            }
        }
        Err(e) => inv.errors.push(e),
    }

    let mut labelled_volumes: BTreeSet<String> = BTreeSet::new();
    let fmt = format!("{{{{.Name}}}}\t{{{{.Label \"{COMPOSE_PROJECT_LABEL}\"}}}}");
    for kind in ["volume", "network"] {
        match list(runner, "docker", &[kind, "ls", "--filter", &project_filter, "--format", &fmt]) {
            Ok(out) => {
                for line in out.lines().filter(|l| !l.trim().is_empty()) {
                    let f = fields(line, 2);
                    if kind == "volume" {
                        labelled_volumes.insert(f[0].to_string());
                    }
                    if let Some(p) = project(&mut projects, scope, f[1]) {
                        let bucket = if kind == "volume" { &mut p.named_volumes } else { &mut p.networks };
                        bucket.push(f[0].to_string());
                    }
                }
            }
            Err(e) => inv.errors.push(e),
        }
    }

    for p in projects.values_mut() {
        if p.containers.is_empty() {
            continue;
        }
        let mut a = vec!["inspect", "--format", "{{json .Mounts}}"];
        a.extend(p.containers.iter().map(|(id, _)| id.as_str()));
        match list(runner, "docker", &a) {
            Ok(out) => {
                let mut anon = BTreeSet::new();
                for line in out.lines().filter(|l| !l.trim().is_empty()) {
                    let Ok(mounts) = serde_json::from_str::<serde_json::Value>(line) else {
                        inv.errors.push(format!("cannot parse mounts of project {}: {line}", p.name));
                        continue;
                    };
                    for m in mounts.as_array().into_iter().flatten() {
                        let is_volume = m.get("Type").and_then(|t| t.as_str()) == Some("volume");
                        let name = m.get("Name").and_then(|n| n.as_str()).unwrap_or("");
                        if is_volume && is_anonymous_volume_name(name) && !labelled_volumes.contains(name) {
                            anon.insert(name.to_string());
                        }
                    }
                }
                p.anonymous_volumes = anon.into_iter().collect();
            }
            Err(e) => inv.errors.push(e),
        }
    }
    inv.compose = projects.into_values().collect();
}

/// One removal step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Run { program: String, args: Vec<String> },
    RemoveFile(PathBuf),
}

impl Action {
    fn run(program: &str, a: &[&str]) -> Action {
        Action::Run {
            program: program.to_string(),
            args: args(a),
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Run { program, args } => write!(f, "{program} {}", args.join(" ")),
            Action::RemoveFile(p) => write!(f, "rm {}", p.display()),
        }
    }
}

/// Order the removals for `inv`. See the module docs for the order.
pub fn plan(inv: &Inventory) -> Vec<Action> {
    let mut out = Vec::new();
    let mut leftover_containers: Vec<&String> = Vec::new();
    if inv.k3d_available {
        for r in &inv.k3d_registries {
            out.push(Action::run("k3d", &["registry", "delete", r]));
        }
        for c in &inv.k3d_clusters {
            out.push(Action::run("k3d", &["cluster", "delete", c]));
        }
    } else {
        leftover_containers.extend(&inv.k3d_registries);
    }
    leftover_containers.extend(&inv.k3d_containers);
    for c in leftover_containers {
        out.push(Action::run("docker", &["rm", "-f", "-v", c]));
    }
    for n in &inv.k3d_networks {
        out.push(Action::run("docker", &["network", "rm", n]));
    }
    for v in &inv.k3d_volumes {
        out.push(Action::run("docker", &["volume", "rm", v]));
    }
    for p in &inv.kubeconfig_files {
        out.push(Action::RemoveFile(p.clone()));
    }
    for c in &inv.kube_contexts {
        out.push(Action::run("kubectl", &["config", "delete-context", c]));
    }
    for c in &inv.kube_clusters {
        out.push(Action::run("kubectl", &["config", "delete-cluster", c]));
    }
    for u in &inv.kube_users {
        out.push(Action::run("kubectl", &["config", "delete-user", u]));
    }
    for p in &inv.compose {
        for (id, _) in &p.containers {
            out.push(Action::run("docker", &["rm", "-f", id]));
        }
        for v in p.anonymous_volumes.iter().chain(&p.named_volumes) {
            out.push(Action::run("docker", &["volume", "rm", v]));
        }
        for n in &p.networks {
            out.push(Action::run("docker", &["network", "rm", n]));
        }
    }
    out
}

/// Result of one [`Action`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Removed,
    /// The object was already gone (an earlier step removed it).
    AlreadyGone,
    Failed(String),
}

/// What [`execute`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub steps: Vec<(Action, Outcome)>,
}

impl SweepReport {
    pub fn failures(&self) -> Vec<&(Action, Outcome)> {
        self.steps.iter().filter(|(_, o)| matches!(o, Outcome::Failed(_))).collect()
    }
}

fn already_gone(out: &CmdOutput) -> bool {
    let e = out.stderr.to_ascii_lowercase();
    ["no such", "not found", "does not exist", "not in "].iter().any(|p| e.contains(p))
}

/// Run `actions` in order. A failed step does not stop later steps. The
/// `HostLock` parameter proves the caller holds the host-wide lock.
pub fn execute(runner: &dyn CommandRunner, actions: &[Action], _lock: &HostLock) -> SweepReport {
    let mut report = SweepReport::default();
    for action in actions {
        let outcome = match action {
            Action::RemoveFile(p) => match std::fs::remove_file(p) {
                Ok(()) => Outcome::Removed,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Outcome::AlreadyGone,
                Err(e) => Outcome::Failed(e.to_string()),
            },
            Action::Run { program, args } => match runner.run(program, args) {
                Ok(out) if out.success() => Outcome::Removed,
                Ok(out) if already_gone(&out) => Outcome::AlreadyGone,
                Ok(out) => Outcome::Failed(first_line(&out.stderr).to_string()),
                Err(e) => Outcome::Failed(format!("could not start {program}: {e}")),
            },
        };
        report.steps.push((action.clone(), outcome));
    }
    report
}

/// [`discover`], [`plan`] and, unless `dry_run`, [`execute`].
pub fn sweep(
    runner: &dyn CommandRunner,
    scope: &Scope,
    tmp_dir: &Path,
    lock: &HostLock,
    dry_run: bool,
) -> (Inventory, Vec<Action>, SweepReport) {
    let inv = discover(runner, scope, tmp_dir);
    let actions = plan(&inv);
    let report = if dry_run {
        SweepReport::default()
    } else {
        execute(runner, &actions, lock)
    };
    (inv, actions, report)
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::cell::RefCell;

    /// Canned replies keyed by `"<program> <args…>"` prefix; records every
    /// call. Unknown commands succeed with empty stdout.
    #[derive(Default)]
    pub struct FakeRunner {
        pub replies: Vec<(String, std::io::Result<CmdOutput>)>,
        pub calls: RefCell<Vec<String>>,
    }

    impl FakeRunner {
        pub fn reply(mut self, prefix: &str, out: CmdOutput) -> Self {
            self.replies.push((prefix.to_string(), Ok(out)));
            self
        }

        pub fn missing(mut self, prefix: &str) -> Self {
            self.replies.push((
                prefix.to_string(),
                Err(std::io::Error::new(std::io::ErrorKind::NotFound, "not on PATH")),
            ));
            self
        }

        pub fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, args: &[String]) -> std::io::Result<CmdOutput> {
            let line = format!("{program} {}", args.join(" "));
            self.calls.borrow_mut().push(line.clone());
            for (prefix, reply) in &self.replies {
                if line.starts_with(prefix.as_str()) {
                    return match reply {
                        Ok(o) => Ok(o.clone()),
                        Err(e) => Err(std::io::Error::new(e.kind(), e.to_string())),
                    };
                }
            }
            Ok(CmdOutput::ok(""))
        }
    }

    /// A host with two e2e k3d leftovers, one e2e compose project, and
    /// look-alikes that must never match.
    pub fn busy_host() -> FakeRunner {
        let anon = "a".repeat(64);
        let dev_anon = "b".repeat(64);
        FakeRunner::default()
            .reply(
                "k3d cluster list",
                CmdOutput::ok(
                    r#"[{"name":"firestream-e2e-redis-abc123"},{"name":"firestream-dev"},{"name":"mycluster"}]"#,
                ),
            )
            .reply(
                "docker ps -a --filter label=app=k3d",
                CmdOutput::ok(
                    "k3d-firestream-e2e-redis-abc123-server-0\tk3d\tserver\n\
                     k3d-firestream-e2e-redis-abc123-serverlb\tk3d\tloadbalancer\n\
                     k3d-firestream-e2e-redis-abc123-registry\tk3d\tregistry\n\
                     k3d-firestream-e2e-fake-zzz999-server-0\t\tserver\n\
                     k3d-firestream-dev-server-0\tk3d\tserver\n",
                ),
            )
            .reply(
                "docker network ls --filter label=app=k3d",
                CmdOutput::ok(
                    "k3d-firestream-e2e-redis-abc123\tk3d\n\
                     k3d-firestream-e2e-supabase-rkpzmb\tk3d\n\
                     k3d-firestream-e2e-lifecycle-5u750j\tk3d\n\
                     k3d-firestream-e2e-unlabelled-qqq111\t\n\
                     k3d-mycluster\tk3d\n",
                ),
            )
            .reply(
                "docker volume ls --filter label=app=k3d",
                CmdOutput::ok("k3d-firestream-e2e-redis-abc123-images\tk3d\nk3d-firestream-images\tk3d\n"),
            )
            .reply("kubectl config get-contexts", CmdOutput::ok("k3d-firestream-e2e-redis-abc123\nk3d-firestream-dev\nminikube\n"))
            .reply("kubectl config get-clusters", CmdOutput::ok("NAME\nk3d-firestream-e2e-redis-abc123\nk3d-firestream-dev\n"))
            .reply("kubectl config get-users", CmdOutput::ok("NAME\nadmin@k3d-firestream-e2e-redis-abc123\nadmin@k3d-firestream-dev\n"))
            .reply(
                "docker ps -a --filter label=com.docker.compose.project",
                CmdOutput::ok(
                    "c1\tfirestream-e2e-airflow-def456-postgres-1\tfirestream-e2e-airflow-def456\n\
                     c2\tfirestream-airflow-postgres-1\tfirestream-airflow\n\
                     c3\tconceptdb-odoo-e2e-odoo-1\tconceptdb-odoo-e2e\n\
                     c4\tfirestream-e2e\tfirestream-e2e\n",
                ),
            )
            .reply(
                "docker volume ls --filter label=com.docker.compose.project",
                CmdOutput::ok(
                    "firestream-e2e-airflow-def456_pgdata\tfirestream-e2e-airflow-def456\n\
                     firestream-airflow_pgdata\tfirestream-airflow\n\
                     conceptdb-odoo-e2e_odoo_data\tconceptdb-odoo-e2e\n",
                ),
            )
            .reply(
                "docker network ls --filter label=com.docker.compose.project",
                CmdOutput::ok(
                    "firestream-e2e-airflow-def456_default\tfirestream-e2e-airflow-def456\n\
                     firestream-airflow_default\tfirestream-airflow\n",
                ),
            )
            .reply(
                "docker inspect --format {{json .Mounts}} c1",
                CmdOutput::ok(format!(
                    r#"[{{"Type":"volume","Name":"{anon}"}},{{"Type":"volume","Name":"firestream-e2e-airflow-def456_pgdata"}},{{"Type":"volume","Name":"shared-external"}},{{"Type":"bind","Source":"/tmp"}},{{"Type":"volume","Name":"{dev_anon}x"}}]"#
                )),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::fake::*;
    use super::*;

    fn tmp_with_files(names: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("fs-e2e-core-sweep-test-{}", RunId::random()));
        std::fs::create_dir_all(&d).unwrap();
        for n in names {
            std::fs::write(d.join(n), "x").unwrap();
        }
        d
    }

    fn test_lock() -> (HostLock, PathBuf) {
        let d = tmp_with_files(&[]);
        let lock = HostLock::try_acquire_at(&d.join("lock")).unwrap().unwrap();
        (lock, d)
    }

    #[test]
    fn scope_all_never_matches_dev_or_foreign_names() {
        let s = Scope::All;
        for good in ["firestream-e2e-redis-abc123", "firestream-e2e-x"] {
            assert!(s.matches_name(good), "{good}");
        }
        for bad in [
            "firestream-redis",
            "firestream-airflow",
            "firestream-e2e",
            "firestream-e2e-",
            "conceptdb-odoo-e2e",
            "myfirestream-e2e-redis-abc123",
            "",
        ] {
            assert!(!s.matches_name(bad), "{bad}");
        }
    }

    #[test]
    fn scope_k3d_objects() {
        let s = Scope::All;
        assert!(s.matches_k3d_object("k3d-firestream-e2e-redis-abc123"));
        assert!(s.matches_k3d_object("k3d-firestream-e2e-redis-abc123-server-0"));
        assert!(!s.matches_k3d_object("k3d-firestream-dev-server-0"));
        assert!(!s.matches_k3d_object("firestream-e2e-redis-abc123"));
        assert!(!s.matches_k3d_object("k3d-mycluster"));

        let run = Scope::Run(RunId::parse("abc123").unwrap());
        for good in [
            "k3d-firestream-e2e-redis-abc123",
            "k3d-firestream-e2e-pg-backup-abc123-server-0",
            "k3d-firestream-e2e-redis-abc123-agent-12",
            "k3d-firestream-e2e-redis-abc123-serverlb",
            "k3d-firestream-e2e-redis-abc123-tools",
            "k3d-firestream-e2e-redis-abc123-registry",
            "k3d-firestream-e2e-redis-abc123-images",
        ] {
            assert!(run.matches_k3d_object(good), "{good}");
        }
        for bad in [
            "k3d-firestream-e2e-redis-abc124",
            "k3d-firestream-e2e-abc123-redis-xyz789",
            "k3d-firestream-e2e-redis-abc123-other",
        ] {
            assert!(!run.matches_k3d_object(bad), "{bad}");
        }

        let one = Scope::for_run(&RunId::parse("abc123").unwrap(), Some("redis"));
        assert!(one.matches_k3d_object("k3d-firestream-e2e-redis-abc123-images"));
        assert!(!one.matches_k3d_object("k3d-firestream-e2e-kafka-abc123-images"));
    }

    #[test]
    fn discover_keeps_only_harness_objects() {
        let runner = busy_host();
        let tmp = tmp_with_files(&[
            "firestream-e2e-redis-abc123.kubeconfig",
            "firestream-dev.kubeconfig",
            "firestream-e2e-redis-abc123.txt",
        ]);
        let inv = discover(&runner, &Scope::All, &tmp);
        assert!(inv.k3d_available);
        assert!(inv.errors.is_empty(), "{:?}", inv.errors);
        assert_eq!(inv.k3d_clusters, ["firestream-e2e-redis-abc123"]);
        assert_eq!(inv.k3d_registries, ["k3d-firestream-e2e-redis-abc123-registry"]);
        assert_eq!(
            inv.k3d_containers,
            ["k3d-firestream-e2e-redis-abc123-server-0", "k3d-firestream-e2e-redis-abc123-serverlb"]
        );
        assert_eq!(
            inv.k3d_networks,
            [
                "k3d-firestream-e2e-redis-abc123",
                "k3d-firestream-e2e-supabase-rkpzmb",
                "k3d-firestream-e2e-lifecycle-5u750j"
            ]
        );
        assert_eq!(inv.k3d_volumes, ["k3d-firestream-e2e-redis-abc123-images"]);
        assert_eq!(inv.kubeconfig_files, [tmp.join("firestream-e2e-redis-abc123.kubeconfig")]);
        assert_eq!(inv.kube_contexts, ["k3d-firestream-e2e-redis-abc123"]);
        assert_eq!(inv.kube_clusters, ["k3d-firestream-e2e-redis-abc123"]);
        assert_eq!(inv.kube_users, ["admin@k3d-firestream-e2e-redis-abc123"]);
        assert_eq!(inv.compose.len(), 1);
        let p = &inv.compose[0];
        assert_eq!(p.name, "firestream-e2e-airflow-def456");
        assert_eq!(p.containers, [("c1".to_string(), "firestream-e2e-airflow-def456-postgres-1".to_string())]);
        assert_eq!(p.anonymous_volumes, ["a".repeat(64)]);
        assert_eq!(p.named_volumes, ["firestream-e2e-airflow-def456_pgdata"]);
        assert_eq!(p.networks, ["firestream-e2e-airflow-def456_default"]);
        for o in inv.objects() {
            assert!(!o.contains("firestream-airflow"), "{o}");
            assert!(!o.contains("firestream-dev"), "{o}");
            assert!(!o.contains("conceptdb"), "{o}");
            assert!(!o.contains("unlabelled"), "{o}");
            assert!(!o.contains("zzz999"), "{o}");
            assert!(!o.contains("shared-external"), "{o}");
        }
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn discover_by_run_selects_one_run() {
        let runner = busy_host();
        let tmp = tmp_with_files(&[]);
        let inv = discover(&runner, &Scope::Run(RunId::parse("rkpzmb").unwrap()), &tmp);
        assert_eq!(inv.objects(), ["k3d network k3d-firestream-e2e-supabase-rkpzmb"]);
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn plan_orders_k3d_then_compose() {
        let runner = busy_host();
        let tmp = tmp_with_files(&["firestream-e2e-redis-abc123.kubeconfig"]);
        let inv = discover(&runner, &Scope::Run(RunId::parse("abc123").unwrap()), &tmp);
        let steps: Vec<String> = plan(&inv).iter().map(|a| a.to_string()).collect();
        let kc = tmp.join("firestream-e2e-redis-abc123.kubeconfig");
        assert_eq!(
            steps,
            [
                "k3d registry delete k3d-firestream-e2e-redis-abc123-registry".to_string(),
                "k3d cluster delete firestream-e2e-redis-abc123".to_string(),
                "docker rm -f -v k3d-firestream-e2e-redis-abc123-server-0".to_string(),
                "docker rm -f -v k3d-firestream-e2e-redis-abc123-serverlb".to_string(),
                "docker network rm k3d-firestream-e2e-redis-abc123".to_string(),
                "docker volume rm k3d-firestream-e2e-redis-abc123-images".to_string(),
                format!("rm {}", kc.display()),
                "kubectl config delete-context k3d-firestream-e2e-redis-abc123".to_string(),
                "kubectl config delete-cluster k3d-firestream-e2e-redis-abc123".to_string(),
                "kubectl config delete-user admin@k3d-firestream-e2e-redis-abc123".to_string(),
            ]
        );

        let inv = discover(&runner, &Scope::Run(RunId::parse("def456").unwrap()), &tmp);
        let steps: Vec<String> = plan(&inv).iter().map(|a| a.to_string()).collect();
        assert_eq!(
            steps,
            [
                "docker rm -f c1".to_string(),
                format!("docker volume rm {}", "a".repeat(64)),
                "docker volume rm firestream-e2e-airflow-def456_pgdata".to_string(),
                "docker network rm firestream-e2e-airflow-def456_default".to_string(),
            ]
        );
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn plan_without_k3d_uses_docker_for_registry() {
        let runner = busy_host().missing("k3d ");
        let runner = FakeRunner {
            replies: runner.replies.into_iter().rev().collect(),
            ..Default::default()
        };
        let tmp = tmp_with_files(&[]);
        let inv = discover(&runner, &Scope::Run(RunId::parse("abc123").unwrap()), &tmp);
        assert!(!inv.k3d_available);
        assert_eq!(inv.warnings.len(), 1, "{:?}", inv.warnings);
        let steps: Vec<String> = plan(&inv).iter().map(|a| a.to_string()).collect();
        assert!(steps.iter().all(|s| !s.starts_with("k3d ")), "{steps:?}");
        assert_eq!(steps[0], "docker rm -f -v k3d-firestream-e2e-redis-abc123-registry");
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn execute_runs_in_order_and_tolerates_gone_objects() {
        let runner = busy_host()
            .reply("docker rm -f -v", CmdOutput::failed("Error: No such container: x"))
            .reply("docker network rm", CmdOutput::failed("error: network has active endpoints"));
        let tmp = tmp_with_files(&["firestream-e2e-redis-abc123.kubeconfig"]);
        let (lock, lock_dir) = test_lock();
        let (_, actions, report) = sweep(
            &runner,
            &Scope::Run(RunId::parse("abc123").unwrap()),
            &tmp,
            &lock,
            false,
        );
        let executed: Vec<String> = runner
            .calls()
            .into_iter()
            .filter(|c| !c.contains(" ls ") && !c.contains(" list ") && !c.contains("ps -a") && !c.contains("config get-"))
            .collect();
        let expected: Vec<String> = actions
            .iter()
            .filter(|a| matches!(a, Action::Run { .. }))
            .map(|a| a.to_string())
            .collect();
        assert_eq!(executed, expected);
        assert_eq!(report.steps.len(), actions.len());
        let failures = report.failures();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].0.to_string().starts_with("docker network rm"));
        assert!(report.steps.iter().any(|(_, o)| *o == Outcome::AlreadyGone));
        assert!(!tmp.join("firestream-e2e-redis-abc123.kubeconfig").exists());
        let _ = std::fs::remove_dir_all(tmp);
        drop(lock);
        let _ = std::fs::remove_dir_all(lock_dir);
    }

    #[test]
    fn dry_run_executes_nothing() {
        let runner = busy_host();
        let tmp = tmp_with_files(&["firestream-e2e-redis-abc123.kubeconfig"]);
        let (lock, lock_dir) = test_lock();
        let (_, actions, report) = sweep(&runner, &Scope::All, &tmp, &lock, true);
        assert!(!actions.is_empty());
        assert!(report.steps.is_empty());
        assert!(runner.calls().iter().all(|c| !c.contains(" rm ") && !c.contains("delete")));
        assert!(tmp.join("firestream-e2e-redis-abc123.kubeconfig").exists());
        let _ = std::fs::remove_dir_all(tmp);
        drop(lock);
        let _ = std::fs::remove_dir_all(lock_dir);
    }

    #[test]
    fn docker_failure_is_an_error() {
        let runner = FakeRunner::default().reply("docker ", CmdOutput::failed("Cannot connect to the Docker daemon"));
        let tmp = tmp_with_files(&[]);
        let inv = discover(&runner, &Scope::All, &tmp);
        assert!(!inv.errors.is_empty());
        assert!(inv.errors[0].contains("Cannot connect"));
        let _ = std::fs::remove_dir_all(tmp);
    }
}
