# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Firestream is a serverless data warehouse designed as "create-react-app for data engineering" - providing a fully local K3D cluster that can be deployed to production in GCP or AWS. It combines the Dataflow paradigm with Kubernetes infrastructure as code, emphasizing minimal data movement throughout its lifecycle.

### Core Concept
- Local development with K3D (Kubernetes in Docker) that mirrors production environments
- Infrastructure as Code via DevContainer with Docker-from-Docker pattern
- Supports real-time streaming data (webhooks, websockets, REST) with streaming effects
- Built 100% on open source (Bitnami charts, Apache projects, K8s ecosystem)

## Architecture

### Multi-Language Stack
1. **Rust** (Primary): TUI interface, API server, and infrastructure tooling
2. **Python**: ETL library, Airflow DAGs, data processing
3. **Scala**: Spark stateful streaming applications
4. **Node.js**: Middleware services, dashboard frontends
5. **Nix**: Reproducible build environment and dependency management

### Key Components

#### Rust Workspace (`src/lib/rust/`)
- `firestream-tui/`: Terminal UI for resource management (default workspace member)
- `firestream-api-server/`: Web API with PostgreSQL backend (multi-crate Gerust architecture)
  - `cli/`: DB migrations and code generation
  - `config/`: Environment-specific configuration management
  - `db/`: Database entities, migrations, access layer
  - `web/`: Web interface, controllers, auth middleware
  - `macros/`: Test macros
- `tools/`: Infrastructure management utilities
  - `k8s-manager`: Kubernetes resource operations
  - `helm-manager`: helm-CLI execution layer (helm/kubectl shell-out + values resolver; no embedded charts)
  - `firestream-charts`: chart registry — reads the flake-emitted index at `/opt/firestream/charts`
  - `docker-manager`: Container management
  - `templatizer-spark`: Spark application templating
  - `templatizer-puppeteer`: Node.js app templating
  - `templatizer-superset`: Superset dashboard templating
  - `filesystem-manager`: File system utilities

#### Python Libraries (`src/lib/python/`)
- `etl_lib/`: Core ETL utilities for Spark, S3, Delta Lake
- `configurator/`: Infrastructure configuration management
- `gen_proto/`: Generated Protocol Buffer bindings

#### Infrastructure
- **Containers**: Located in `src/containers/` (Bitnami fork)
- **Charts**: Located in `src/charts/` (Bitnami Helm charts fork)
- **Templates**: Project scaffolding in `src/templates/`
  - `standard_project/`: Basic project template
  - `standard_puppeteer/`: Node.js web app template
- **Deployment Packages**: Located in `src/deployment/packages/`

### DevContainer Architecture
The project uses a sophisticated DevContainer setup that:
- Runs K3D cluster on the host's Docker Engine
- Uses IP Tables to resolve Kubernetes internal DNS via CoreDNS
- Allows devcontainer processes to reach `service_name.namespace.svc.cluster.local`
- Essentially a seamless kubectl proxy that works across any host OS

## Development Commands

### Building and Running

```bash
# Build and run the TUI (default workspace member)
cargo run

# Build specific Rust component
cargo run -p firestream-api-server

# Build all Rust workspace members
cargo build --workspace

# Run Rust tests
cargo test --workspace

# Build Rust documentation
cargo doc --workspace --all-features
```

### Environment Setup

```bash
# Initialize development environment (runs bootstrap process)
make development

# Clean rebuild of environment
make development_clean

# Resume existing cluster (re-establish network tunnel)
make resume

# Build services and container registry
make build

# Run tests in CI/CD mode
make test
```

### Bootstrap Process
The `bootstrap.sh` script orchestrates deployment in different modes:
- `development`: Full K3D cluster + Helm charts + port forwarding
- `test`: CI/CD testing mode
- `build`: Build container images and artifacts
- `clean`: Create cluster without services
- `resume`: Reconnect to existing cluster
- `production`: Production deployment (TODO)

Modes execute in sequence:
1. K3D cluster setup (`docker/k3d/bootstrap.sh`)
2. DevContainer Python package installation (`bin/cicd_scripts/bootstrap_devcontainer.sh`)
3. Helm chart installation (`bin/cicd_scripts/helm_install.sh`)
4. Port forwarding setup (`bin/cicd_scripts/port_forward.sh`)

### Nix Environment

```bash
# Build the Nix flake (creates reproducible environment)
make nix-up

# Clean old Nix generations
make nix-fix
```

The Nix flake (`flake.nix`) provides:
- Fixed nixpkgs at release-24.11
- Rust toolchain via rustup
- Python 3.11 with key packages
- Scala, SBT, Java 11, Maven
- Node.js 18, gRPC tools
- Kafka (rdkafka), RocksDB
- LLVM/Clang toolchain
- Bitnami charts (fetched and cached at specific commit)

Environment activation: `source /etc/profile.d/nix-env.sh`

### Direnv Integration

The project uses [direnv](https://direnv.net) for automatic environment activation when entering the project directory.

```bash
# First-time setup (after installing direnv)
direnv allow

# Environment loads automatically on cd
cd /path/to/Firestream
# Output: "Firestream development environment loaded"
```

**How it works:**
- `.envrc` uses `use flake . --impure` to activate the Nix environment
- Loads `etc/.env.local` and `etc/.env.secrets` if they exist
- On macOS, sources `bin/darwin-env.sh` for Xcode/SDK configuration
- Local overrides can be placed in `.envrc.local` (git-ignored)

**Files:**
- `.envrc` - Main direnv configuration
- `.envrc.local.example` - Template for user-specific overrides
- `bin/darwin-env.sh` - macOS-specific environment setup

### Python Development

```bash
# Run Python tests
pytest

# Tests are located in src/lib/python/
# Test discovery: test_*.py files
```

### Docker and Kubernetes

```bash
# Build devcontainer
make build-devcontainer

# Build devcontainer without cache
make build-devcontainer-clean

# Delete all Docker resources (clean slate)
make docker-reset
```

## Working with Specific Components

### Firestream TUI
The TUI is the primary user interface, showing a tree-based resource hierarchy:
- **Navigation**: `j/k` or `↑/↓` (items), `h/l` or `←/→` (panes), `Space` (expand/collapse)
- **Quick Actions**: `/` (search), `:` (command palette), `?` (help), `n` (new), `d` (deploy/delete)
- **Panes**: Resources (left), Details (top right), Logs (bottom right)

The TUI uses a mock backend by default. Set `API_URL` environment variable for real API.

### API Server (Gerust Architecture)
The API server follows a multi-crate pattern:

```bash
# Run the API server
cargo run -p firestream-api-server

# Run database migrations
cargo db

# Generate scaffolding (entities, controllers, tests)
cargo generate

# Run API server tests
cargo test -p firestream-api-server
```

Configuration uses `.env` and `.env.test` files for development and test environments.

### Airflow Development

```bash
# Setup Airflow for local CLI testing
make airflow

# This runs:
# 1. DBT profile configuration
# 2. Local Airflow bootstrap
# 3. Test DAG execution
```

DAGs are located in deployment packages and examples directories.

#### Guest-DAG dependencies: `options.airflow.dagWorkspace`

Guest DAGs that need their **own** Python dependencies declare them as a
**separate uv2nix workspace**, opt-in via `options.airflow.dagWorkspace = { src;
overrides ? null; }` (`src/containers/firestream/airflow/options.nix`). `src` is a
directory holding `pyproject.toml` + `uv.lock` (+ optional `overrides.nix`, auto-loaded
from the dir). It is resolved through the generic `extraWorkspaces` seam on
`eval-container.nix`/`python-workspace.nix` into a **separate baked venv** at
`/opt/firestream/airflow/dags-venv`, isolated from Airflow's own venv
(`/opt/firestream/airflow/venv`) and its resolution closure. Guest deps are still
first-class in downstream Nix builds (they flow through the same wheel+sdist overlays and
appear in the source archive / SBOM). Stock image is byte-identical when unset (same
discipline as `vendoredDags`); `requires-python` must admit `python312` or the factory
throws a legible error.

When configured, two env vars are baked into every Airflow pod:
- `FIRESTREAM_DAGS_VENV=/opt/firestream/airflow/dags-venv`
- `FIRESTREAM_DAGS_PYTHON=/opt/firestream/airflow/dags-venv/bin/python`

Airflow's scheduler/dag-processor parse DAGs with Airflow's **own** interpreter, so the
guest venv is deliberately kept off their `PYTHONPATH`. DAGs reach it across a process
boundary via two tiers:
- **Tier A / recommended** — `BashOperator` (or `@task.bash`) running
  `$FIRESTREAM_DAGS_VENV/bin/<console-script>`: separate process, guest venv needs nothing
  from Airflow. (BashOperator pushes only the *last* stdout line to XCom — emit a single
  final line or write a file for structured output.) `@task.external_python(python=os.environ["FIRESTREAM_DAGS_PYTHON"])`
  with **no** `apache-airflow` in the guest is also Tier A: serializable-args-in /
  return-value-out, no live Airflow context.
- **Tier B** — the guest pins `apache-airflow==3.0.3` in `external_python` to regain live
  context + in-callable XCom, at the cost of re-coupling to Airflow's closure and
  image/build-time blowup. A deliberate tradeoff, not free.

Copy-and-adapt fixture (a working example workspace + DAG showing both shapes):
`src/templates/airflow_dags_workspace/`.

#### In-process Python dependencies: `options.<app>.pythonWorkspace`

`dagWorkspace`/`extraWorkspaces` build a **separate** venv, which only helps a guest that runs
across a process boundary. An app that imports the packages **itself** (Odoo addons, Superset
plugins, JupyterHub authenticators) needs them in the **primary** venv. The generic
`options.<app>.pythonWorkspace` option (`bin/nix/firestream/containers/eval-container.nix`) does
that for every python-workspace container; no per-app `module.nix` change is needed because the
module receives a finished `pythonEnv`.

- **`extend = [ { src; overrides ? null; } ]`** — consumer uv2nix workspaces declaring only *new*
  packages. The factory (`python-workspace.nix`) composes extension package overlays **before** the
  base overlay (base-last keeps Firestream's per-package build config), builds **one** venv from
  the union of `deps.default` member maps, and first runs `assertCompatibleLocks`
  (`python-workspace-lib.nix`): any shared package at a different version, or a member name
  colliding with the base project, throws with the package names and both versions.
  pyproject.nix itself performs no version validation, so this guard is load-bearing — never
  downgrade it to a warning.
- **`replace = { src; overrides ? null; inheritOverrides ? true; }`** — the consumer's
  `pyproject.toml` + `uv.lock` become the primary workspace root (copy the app's, edit,
  `uv lock`). `module.nix` still loads from Firestream's `workspacePath`. Firestream's
  `overrides.nix` (system-library build inputs) composes underneath the consumer's.
- Both unset ⇒ the factory call is textually identical to today; `checks.firestream-python-workspace-seam`
  asserts `pythonEnv`/`dockerImage` drvPath equality on odoo-18 and airflow, and that the fixture
  extends odoo-18 with `pycairo`. Pure lock-diff cases: `checks.firestream-python-workspace-lib-tests`.
- `requires-python` of every root must admit the container's interpreter. The pinned uv2nix does
  NOT honour `[tool.uv.extra-build-dependencies]`; an sdist-only package the extension introduces
  gets its build backend in the extension's `overrides.nix` via `final.resolveBuildSystem`
  (see the fixture's `pycairo` entry).
- Fixture: `src/templates/odoo_python_workspace/` (pycairo, rlPyCairo, freetype-py; deliberately
  not `plaid-python`, which the odoo/18 lock already pins). Example:
  `examples/odoo-python-dependencies/`.

#### Odoo runtime limits

`src/containers/firestream/odoo/options.nix` types every `odoo.conf` limit (`workers`,
`limitTimeCpu`, `limitTimeReal`, `limitTimeRealCron`, `limitMemorySoft`, `limitMemoryHard`,
`limitRequest`, `maxCronThreads`, `listDb`) and bakes them as `ODOO_*` env; defaults equal Odoo's
own. The odoo.conf template lives in `module.nix` and is rendered by **two** generators
(`activateFn` and `scripts/config.sh`) that must stay in step. The chart mirrors them as nullable
typed options in `src/charts/firestream/odoo/nix/options/app.nix`; `templates/deployment.yaml`
emits each env var only when set, so the stock render is unchanged (`odoo-render-fidelity`).

## Data Stack Technologies

### Storage & Catalog
- **MinIO**: S3-compatible object storage
- **Delta Lake**: Table format
- **LakeFS**: Git-like data versioning
- **PostgreSQL**: Relational database (Bitnami chart)

### Processing
- **Apache Spark**: Distributed processing with Spark Operator
- **Kafka**: Message streaming (Bitnami chart)
- **Airflow**: Workflow orchestration

### Visualization
- **Superset**: BI dashboards
- **Plotly.js**: Real-time client-side dashboards

### Networking
- **Contour/Envoy**: Ingress controller
- **CoreDNS**: Internal DNS resolution

## Project Structure Patterns

### Rust Workspace Dependencies
The project uses workspace-level dependencies to ensure version consistency:
- Kubernetes: `k8s-openapi` with `v1_31` feature matching K3D version

### Multi-Crate Internal Dependencies
Rust tools reference each other via workspace paths:
```toml
templatizer-spark = { path = "src/lib/rust/tools/templatizer-spark" }
filesystem-manager = { path = "src/lib/rust/tools/filesystem-manager" }
```

### Python Editable Installs
Python packages in `src/lib/python/` are installed in editable mode during bootstrap for development.

## Key Architectural Decisions

### K3D over Kind
While the README mentions Kind, the project has standardized on K3D:
- Lighter weight than full K3s
- Better suited for local development
- Maintains compatibility with K8s 1.31 (matching `k8s-openapi` feature flag)

### Bitnami Charts Fork
The project maintains a fork of Bitnami charts in `src/charts/` and containers in `src/containers/`:
- Allows customization for Firestream-specific needs
- Ensures version stability
- Bitnami charts commit hash in Nix flake: `9bc801b4caa0b2fff6ae3392f6b417877a056965`

### Helm Chart Contract (Nix -> JSON -> Rust)
The flake is the source of truth for helm. Each chart at `src/charts/firestream/<name>/nix/` is a typed-options overlay using `lib.evalModules`. The shared option types live in `bin/nix/firestream/charts/lib/types/` (image, kubernetes, autoscaling, network-policy, tls, **persistence**, **secrets**); the per-chart evaluator is `bin/nix/firestream/charts/eval-chart.nix`. The forked Bitnami chart YAML (`Chart.yaml`, `templates/`, `values.yaml`) is vendored unmodified-by-Nix under `src/charts/firestream/<name>/` and is the source of truth for templates — Nix never regenerates templates. See the full architecture spec: `docs/firestream-supported-app.md`.

**Manifest emission (schema v1):**
- Each chart emits a `chart-manifest.json` (chart name, version, repo, vendored subchart paths, default `values.yaml`, and a `_meta` block).
- `_meta.containerRefs` is the per-chart image-injection seam. Every chart with a Firestream-built container injects `firestream-*` images via the canonical pattern in `nix/flake-modules/charts/airflow.nix` (lines 75-137); subchart slots (postgresql/redis) ship on airflow, superset, odoo, jupyterhub. `cloudflared` is the exception — `componentPath = [ ]`, catalogue-only.
- Subchart vendoring helper: `bin/nix/firestream/charts/lib/vendor-subcharts.nix`. Values rendering helper: `bin/nix/firestream/charts/lib/to-values-yaml.nix`.

**Aggregate bundle:**
- `packages.firestream-charts-bundle` (see `nix/flake-modules/charts/`) is a symlink farm with a top-level `index.json` listing every chart (`charts`), every base chart (`baseCharts`), and the named stack compositions (`stacks`).
- Default deploy path: `/opt/firestream/charts/`. The dev shell exports `FIRESTREAM_CHARTS_DIR` pointing directly at the bundle's `/nix/store` path (env var only — no out-link is written into the working tree; same for `FIRESTREAM_CI_PROFILE`).
- Charts with typed overlays in `firestreamCharts`: `airflow`, `postgresql`, `redis`, `kafka`, `spark`, `jupyterhub`, `superset`, `odoo`, `seaweedfs`, `nextjs`, `nginx`, `cloudflared`.
- **Two stacks.** `firestreamStacks.dev` is the local data platform (everything above except `cloudflared`, with `nginx` last). `firestreamStacks.edge` is `[ nginx cloudflared ]` — separate because cloudflared's `TUNNEL_TOKEN` secretKeyRef is deliberately non-optional and it deploys `atomic`/`wait`/5m, so on a cluster with no Cloudflare tunnel provisioned it blocks and then rolls back. A local data platform has no business dialling the Cloudflare edge.
- **`cloudflared` is a chart-only app**: no `src/containers/firestream/cloudflared/`, no `firestreamImages.cloudflared`, no compose output. It runs Cloudflare's own image, registered through the `componentPath = [ ]` catalogue-only mode of `_meta.containerRefs` (manifest records it, no values overlay). See `docs/firestream-supported-app.md` §1. The `cloudflared-render-fidelity` check asserts the manifest's triple matches what the chart renders — nothing else ties the two copies together.
- **Backup is a manifest contract.** `_meta.backup = { cronJobSuffix; quiesceDeployment; }` (null by default) is emitted as `backup` in `chart-manifest.json` (`firestream_charts::Backup`). `firestream helm backup|restore <chart>` finds the CronJob as `<bitnami fullname>-<cronJobSuffix>` and refuses charts without the contract. postgresql sets `pgdumpall` (restore streams into the running primary); odoo sets `odoodump` with `quiesceDeployment = true`, so `restore` scales the `<fullname>` Deployment to zero, waits for its pods to go, runs the Job, and scales back (`restore_from_backup` in `cli/commands.rs`, shared with the e2e). Odoo's CronJob is not a chart template: `nix/flake-modules/charts/odoo.nix` appends a templated YAML string to `extraDeploy` guarded by `.Values.backup.enabled`, so `--set backup.enabled=true` works and the stock render is unchanged. The archive format (`manifest.json` + `dump.sql` + `filestore/`) lives in the image (`src/containers/firestream/odoo/scripts/helpers.sh`: `odoo_dump`, `odoo_restore`, `odoo_backup_s3`, `odoo_restore_s3`); the CLI only names the entry point per chart in `restore_command`.
- **Namespace lifecycle is an option.** `_meta.createNamespace` (default `true`) drives BOTH `--create-namespace` on the bundle's `bin/deploy` and `release.createNamespace` in `chart-manifest.json` (which `helm_lifecycle/executor.rs` reads), so the shell path and the Rust path cannot disagree. Set it `false` when the deploying layer owns the namespace — e.g. a Pulumi stack that also attaches the ResourceQuota, LimitRange, NetworkPolicy and Workload Identity ServiceAccount.
- **SeaweedFS is the exception to the Bitnami pattern.** It is a non-Bitnami, Apache-2.0 chart (forked from upstream `seaweedfs/seaweedfs`) whose pods invoke the `weed` binary directly via a `command:` block — so it uses NONE of the Bitnami-compat machinery: no `perContainerHelpers`, no `libhelpers<chart>.sh` emission, no `extraEnvVars` path-remaps, no `firestreamPathOverrides`, and no `global.security.allowInsecureImages` guard. The container is simply "`weed` on PATH". Image injection still uses the canonical `_meta.containerRefs` seam (`componentPath = [ "image" ]` → `.Values.image.{registry,repository,tag}`). SeaweedFS is the **default local S3 object store**: it is deployed FIRST in `firestreamStacks.dev` (object store up before consumers), runs all-in-one single-pod (`weed server -master -volume -filer -s3`) with S3 on 8333, auth on, default bucket `firestream`, creds `firestream`/`firestream-secret`. Its `S3_LOCAL_*` env (`S3_LOCAL_ENDPOINT_URL`, `S3_LOCAL_ACCESS_KEY_ID`, `S3_LOCAL_SECRET_ACCESS_KEY`, `S3_LOCAL_BUCKET_NAME`, `S3_LOCAL_DEFAULT_REGION`) is injected into the spark and airflow charts so Spark/`etl_lib` consume it out of the box (data-driven; no Rust/Python change). Chart data is `emptyDir` by default (ephemeral — fine for dev; a PVC toggle is a production follow-on).

**Two chart outputs per app (both downstream-importable):**
- `packages.<app>-chart` / `firestream.lib.<sys>.charts.<app>.chartBundle` — the **Firestream-overlaid** chart (image injection + path remaps + `chart-manifest.json`). Has the chart nested at `<bundle>/chart`.
- `packages.<app>-base-chart` / `firestream.lib.<sys>.charts.<app>.baseChart` — the **plain forked chart with its own native default `values.yaml`**, no overlay, built by `bin/nix/firestream/charts/lib/base-chart.nix`. `$out` IS the chart dir (Chart.yaml at root), so a consumer can `helm install <release> /nix/store/…-<app>-base-chart` directly. Self-contained (subcharts vendored into the store path).
- The per-app consumer API is `firestream.lib.<sys>.charts.<app> = { chartBundle; baseChart; render; eval; options; }`; `eval` deep-merges consumer overrides onto the Firestream defaults.

**Rust consumers:**
- `firestream-charts` crate (`src/lib/rust/firestream-charts/`): reader for the bundle (`index.json` + per-chart `chart-manifest.json`).
- `helm_lifecycle` module in the `firestream` crate consumes manifests via `chart_info_from_manifest`.
- `helm-manager` crate is strictly the helm/kubectl CLI execution layer (`helm_client`, `kubectl_client`, `values_resolver`, `deployment`); chart embedding (`embedded_charts.rs`, `include_dir`, the `HelmManager` aggregator) is gone.

**CLI:**
```bash
firestream helm deploy <chart>          # deploy a single chart from the bundle
firestream helm deploy-stack <stack>    # deploy a named stack (e.g. dev)
# Override the bundle location:
firestream helm deploy <chart> --charts-dir /custom/path
# or:  FIRESTREAM_CHARTS_DIR=/custom/path firestream helm deploy <chart>
```

**Gotchas:**
- Chart option modules MUST mirror the actual `values.yaml` hierarchy. Bitnami uses both flat (`odoo`) and hub-and-spoke (`superset`, `jupyterhub`) shapes — do not flatten/un-flatten arbitrarily.
- Random-secret normalisation for parity tests lives in `nix/flake-modules/charts/checks.nix` and covers every Bitnami chart family.

**Bitnami compatibility pattern** (how firestream-* containers slot into Bitnami chart templates without forking the chart):

1. **Image substitution.** Each chart's flake-module declares `_meta.containerRefs.<slot> = { registry, repository, tag, componentPath }`; the injector at `bin/nix/firestream/charts/lib/inject-container-images.nix:40-74` writes the triple into `values.yaml` at `componentPath`. Pattern reference: `nix/flake-modules/charts/airflow.nix:117-137`. Subchart slots (`postgresql`, `redis`) use `componentPath = [ "postgresql" "image" ]` to write through to the bundled subchart's values block. Each chart's flake-module also sets `global.security.allowInsecureImages = true` to bypass Bitnami's NOTES.txt image-whitelist refusal.

2. **Path remap (`extraEnvVars`).** Bitnami chart pods mount config/data emptyDirs and PVCs at `/opt/bitnami/<chart>/{conf,tmp,logs}` and `/bitnami/<chart>/...`. Firestream containers bake `*_DIR` env vars pointing at `/opt/firestream/<chart>/...`. Each chart's flake-module declares a `firestreamPathOverrides` list and writes it into the chart's `extraEnvVars` (per-component on multi-pod charts) so the K8s-injected env wins. Canonical reference: `nix/flake-modules/charts/postgresql.nix:72-101`. Coverage of which env vars need remapping is the per-chart inventory in the plan's Phase F log.

3. **Helper visibility.** Bitnami chart init containers source `/opt/bitnami/scripts/lib<chart>.sh` then immediately call helpers like `kafka_server_conf_set`, `airflow_conf_set`, `postgresql_execute`. The firestream engine emits these helpers at TOP-LEVEL of `/opt/firestream/scripts/libhelpers<chart>.sh` via `perContainerHelpers` (parameter on `mkAppModule` in `bin/nix/firestream/apps/base.nix:101-200`). The container build creates a symlink `/opt/bitnami/scripts -> /opt/firestream/scripts` (`bin/nix/firestream/containers/base.nix:362-376`) so Bitnami's `source` lines Just Work. To add a helper that a chart unexpectedly needs: extend `src/containers/firestream/<chart>/scripts/helpers.sh` (function defs ONLY, no side-effects) — the file is `builtins.readFile`-d into the `<chart>Helpers` Nix string in the chart's `module.nix` and then passed via `perContainerHelpers`.

4. **Env defaults semantics.** `bin/nix/firestream/env/defaults.nix:55-59` emits `export VAR="${VAR:-default}"` so K8s-injected env wins over container-baked defaults. Without this, the chart's `extraEnvVars` remaps would be silently overwritten when env-defaults.sh runs.

5. **State-dir tolerance.** Bitnami chart pods often run with `readOnlyRootFilesystem: true` and no PVC at `/firestream`. The shared state-tracking helpers `mark_app_initialized` (`bin/nix/firestream/lib/persistence.nix:123-148`), `save_config_hash`, `record_activation`, and `increment_generation` (`bin/nix/firestream/lib/state.nix:93-263`) silently skip on `EROFS`. Per-container `activateFn`s do NOT need to wrap these calls — the lib is tolerant.

6. **Subchart paths.** When a chart bundles postgresql/redis as subcharts, the parent chart's flake-module must ALSO inject `extraEnvVars` for the subchart (under `config.<chart>.postgresql.primary.extraEnvVars`), or the subchart pod uses its baked `/opt/firestream/postgresql/*` paths and hits read-only mounts. Pattern: `nix/flake-modules/charts/odoo.nix` (lines added in Phase G).

### K8s/Helm E2E Harness

The `firestream-e2e-k8s` crate (`src/lib/rust/firestream-e2e-k8s/`) provides per-chart fresh-cluster k3d e2e tests that drive the same `deploy_chart_lifecycle` path the CLI uses. Sister crate to `firestream-e2e-core` (shared primitives) and the docker `firestream/tests/e2e.rs` harness.

**Run via make**:
- `make test-e2e-k8s` — full 10-chart sweep, serialized, fresh cluster per chart.
- `make test-e2e-k8s-<chart>` — single chart (postgresql/redis/kafka/airflow/spark/jupyterhub/superset/odoo/seaweedfs/nginx). `make test-e2e-k8s-pg-backup` and `make test-e2e-k8s-odoo-backup` are the two multi-chart backup/restore round-trips (seaweedfs + the app). No `cloudflared` arm: the connector reports Ready only once it registers with the Cloudflare edge, so it cannot come up in a hermetic cluster.

**Env contract** (defaults in parens):
- `FIRESTREAM_E2E_K8S_STACKS=all|csv` — subset filter (canonical 10)
- `FIRESTREAM_E2E_K8S_KEEP=1` — skip teardown (unset)
- `FIRESTREAM_E2E_K8S_STRICT=1` — skip-gate → hard fail (unset)
- `FIRESTREAM_E2E_K8S_TIMEOUT_SECS` — per-chart deadline (600)
- `FIRESTREAM_E2E_K8S_PRELOAD=0` — skip Nix image preload (1)
- `FIRESTREAM_E2E_K8S_CHARTS_DIR` — bundle path override (falls through to `FIRESTREAM_CHARTS_DIR`, then `/opt/firestream/charts`)
- `FIRESTREAM_E2E_K8S_HELM_TIMEOUT` — emergency override only; the chart manifest's `deployment.timeout` is authoritative

**Out of scope**: `firestream-healthd` is embedded in container images but not exposed in chart templates (no `9180` port in airflow chart Service). The k8s harness relies on native chart readiness probes + per-protocol probes (postgres `pg_isready`, redis `redis-cli ping`, etc.); wiring healthd into chart Services is a separate RFC. Not in `.github/workflows/build.yaml` — see "E2E as advisory pipeline phases" below for how it *is* reachable from the pipeline.

### E2E as advisory pipeline phases (`--mode e2e`)

Both harnesses above (the k8s one, and the docker one at `src/lib/rust/firestream/tests/e2e.rs` driven by `make test-e2e` / `make test-e2e-<stack>`) are ALSO declared as CI-profile phases. This is **purely additive**: every `make test-e2e*` target and every `FIRESTREAM_E2E*` env var keeps working byte-identically, and is still the right tool for incident reproduction.

```bash
make ci-e2e-dry-run     # print the chain; executes nothing
make ci-e2e             # run it — HOURS, 11 k3d clusters, 8 compose stacks
FIRESTREAM_E2E_STRICT=1 FIRESTREAM_E2E_K8S_STRICT=1 make ci-e2e   # unattended
```

**Shape.** `bin/nix/firestream/ci/profile.nix` declares 20 phases — `e2e-docker-<stack>` ×8 then `e2e-k8s-<chart>` ×12 (the canonical 10 plus `pg-backup` and `odoo-backup`) — each **advisory**, each holding exactly **one** shell task that shells out to the corresponding makefile target. No Rust changed; this is entirely the `shellTasks` seam Phase 6 added.

**Why chained phases and not parallel tasks in one phase.** `firestream_ci::pipeline::Pipeline::run` fans a phase's tasks out with `join_all` and **no concurrency cap**. Both harnesses hold a process-wide mutex (`harness_lock()` / `e2e_lock()`) precisely because they create real k3d clusters and bind host ports; k3d name/port collisions are already engineered away (random cluster suffix, `127.0.0.1` API bind, `pick_ephemeral_port()`), so N concurrent runs would not *collide* — they would just be N k3s servers plus N full data stacks on one machine. So the serialisation is done at profile level: one task per phase, phases chained through `dependsOn`. Phases run strictly one at a time.

**Two `dependsOn` edges per link, and the second is load-bearing.** Each link names its predecessor *and* `verify` directly. Skip is **not transitive** in `Pipeline::run`: a phase is skipped only when one of its **own** `depends_on` is a *required* phase that failed, and a skipped `PhaseOutcome` reports `ok() == true` (an `all()` over zero tasks). With a chain-only wiring, a red `verify` would skip only the head link and then run 17 remaining hours of e2e against a broken tree.

**Failure semantics.** Advisory ⇒ a red chart yields exit **2** (`PartiallyPassed`), never 1, and does **not** stop the sweep — matching the cargo harness, where each chart is its own `#[test]`. A red `verify` (required) skips the whole chain and exits 1.

**Gating.** `modes = [ "e2e" ]`. `make ci` (release) and `make ci-check` do not merely skip these phases, they never materialise them — confirm with `make ci-dry-run`, which lists them as `SKIPPED (modes=e2e)`. The chain is also Linux-gated in `profile.nix`, so the Darwin manifest keeps its "zero runnable phases" property.

**Invariants are tested, not asserted:** `src/util/firestream-ci/tests/e2e_phase_chain.rs` (12 tests) proves advisory⇒exit 2, required⇒exit 1 + chain skipped, one-shell-task-per-phase, "no two shell phases are mutually independent in the DAG", and the direct-gate-edge rule — all from a *synthetic* profile fixture, so `src/util` still learns no Firestream phase names. Two of the twelve re-run the structural invariants against `$FIRESTREAM_CI_PROFILE` when it is set (the dev shell exports it), which is the anti-drift check against the real manifest.

### Docker-from-Docker Pattern
Rather than Docker-in-Docker, the devcontainer binds to `/var/run/docker.sock`:
- Shares host's Docker Engine
- More efficient (no nested virtualization)
- Enables K3D cluster on host while developing in container

### Stateful Streaming Emphasis
The project specifically implements Wiener process demonstrations:
- Spark Structured Streaming generates monotonic sensor data
- Scala MapGroupsWithState merges N-1 and Nth records
- Python stateful streaming as performance comparison
- Node middleware bridges Kafka to WebSocket for browser clients

## Environment Variables

Key variables set by `bootstrap.sh`:
- `DEPLOYMENT_MODE`: One of `development`, `test`, `build`, `clean`, `resume`, `production`
- `MACHINE_ID`: Host machine identifier
- `GIT_COMMIT_HASH`: Current commit for container tagging
- `CPU_ARCHITECTURE`: Detected architecture (x86_64, aarch64)
- `TOTAL_CPU_RESOURCES`: Available CPU cores
- `TOTAL_MEMORY_RESOURCES`: Available memory in MB
- `HAS_NVIDIA_GPU`: Boolean for GPU detection
- `LOCAL_WORKSPACE_FOLDER`: Original host directory for Docker-from-Docker volume mounts

## CI/CD

Two GitHub Actions workflows, plus one helper script:

| File | Trigger | Purpose |
|---|---|---|
| `.github/workflows/build.yaml` | PRs to `main`/`nightly`; pushes to `main`/`nightly`/`staging`/`dev`; `workflow_dispatch` | The merge gate. Cheap by design. |
| `.github/workflows/nightly.yaml` | `schedule` (09:00 UTC) + `workflow_dispatch` | The expensive release pipeline (`make ci`). |
| `.github/scripts/free-disk-space.sh` | called by both | Reclaims ~25 GB of preinstalled toolchains so a Nix store fits in a hosted runner's ~14 GB. |

### `build.yaml` jobs

| Job | Runner | Runs |
|---|---|---|
| `rust` | hosted | `make build`, then `cargo test --workspace --exclude firestream-api-server-web` |
| `util` | hosted | `make test-strategy-parity`, `make test-registry-parity`, `make test-util` |
| `ci-profile` | hosted | `nix build .#firestream-ci-profile .#firestream-ci`, then `make ci-dry-run` |
| `ci-check` | `vars.FIRESTREAM_CI_RUNNER` (gated) | `nix develop --command make ci-check` — the real pipeline, check mode |
| `gate` | hosted | Single required status; treats a skipped `ci-check` as *not-configured*, not green |

Job timeout is 45 minutes (the number the old docs promised), except `ci-check`
(120) and the nightly `release` job (360).

**`cargo test --workspace` is not run verbatim.** `firestream-api-server-web`'s
tests are `#[db_test]`s that fork a database per test and need a live Postgres
plus applied migrations; the package is excluded until that service is stood up
in CI. The e2e suites are `#[ignore]`d and so are already out; Phase 9 made them
reachable as advisory phases under a separate `--mode e2e` (see "E2E as advisory
pipeline phases" above), which no `build.yaml` job invokes.

### Exit-code contract (`firestream_ci::pipeline::Verdict`)

Both workflows encode the same tri-state, and getting it wrong defeats the
design:

| Exit | Meaning | CI behaviour |
|---|---|---|
| `0` | `Passed` | green |
| `1` | `Failed` — a **Required** task failed | **fails the job** |
| `2` | `PartiallyPassed` — every Required passed, ≥1 **Advisory** failed | `::warning::` + job summary, **does not fail** |

Exit 2 is what lets slow or flaky work (`tidy`, `attest`, and the 20 `e2e-*`
phases) live in the pipeline without blocking merges.

**"Continuous but non-blocking" vs. "hosted runners can't do this".** Both are
true and they resolve at the *runner*, not at the tier. Advisory tier is what
makes an e2e sweep safe to run continuously — worst case exit 2, a `::warning::`.
But a sweep that creates 10 k3d clusters cannot execute on a hosted runner at
all, which is why `ci-check` and the nightly `release` job are already gated on
`vars.FIRESTREAM_CI_RUNNER`. So Phase 9 ships the *mechanism* (a mode, a chain,
an advisory tier, a proven exit-2 path) and deliberately ships **no workflow
change**: `.github/` is Phase 8's, and adding an `e2e` job before a runner exists
to run it would only produce a permanently-skipped job. Wiring it up is one job
block — `runs-on: ${{ vars.FIRESTREAM_CI_RUNNER }}`, `nix develop --command make
ci-e2e`, `FIRESTREAM_E2E{,_K8S}_STRICT=1`, the existing exit-2 handling verbatim
— on a schedule slower than nightly.

**Caveat encoded in the workflows:** `ci-linux`'s devshell-sentinel guard also
returns `2`, *before* any phase runs. That is a misconfiguration, not an advisory
failure, so both workflows grep the captured log for
`must run inside the Nix devshell` and hard-fail on it.

### Runner configuration

The release-mode `build` phase (11 container images + 10 charts) does **not** fit
on a GitHub-hosted runner — hours of wall clock, multi-GB outputs, ~14 GB disk
and 7 GB RAM. Two repository variables control this:

- `FIRESTREAM_CI_RUNNER` — runner label for `ci-check` and the nightly `release`
  job. Unset ⇒ `ci-check` is skipped (and reported as such) and `nightly` runs a
  visible *not configured* job instead of pretending.
- `FIRESTREAM_CI_FREE_DISK=1` — run the disk-cleanup script on the nightly
  runner. Only for ephemeral runners; it prunes the Docker image cache, which a
  `STRATEGY=docker` warm cache depends on.

### Preconditions

1. **Nix only sees git-tracked files.** `nix build .#firestream-ci-profile` fails
   with `Path 'nix/flake-modules/util.nix' ... is not tracked by Git` until
   `src/util/`, `bin/nix/firestream/ci/`,
   `nix/flake-modules/{util,ci-profile}.nix`, `bin/build/strategy*.{sh,json}`,
   `bin/build/registry-*.{sh,json}` and `.github/` are `git add`ed.
2. Nix jobs use `fetch-depth: 0` because the flake reads `inputs.self.rev`.
3. `make ci` / `ci-check` must run inside `nix develop` — that is what supplies
   `firestream-ci`, `nix-eval-jobs` and `FIRESTREAM_CI_PROFILE`.
4. Store caching uses `nix-community/cache-nix-action` (GitHub's own cache, no
   secret, 10 GB repo cap). A real binary cache (Cachix / attic / FlakeHub) is
   the eventual answer for the release path.

### Local equivalents

```bash
make ci-dry-run          # build-free: prints the phase DAG, fails on an empty profile
make ci-check            # tidy + verify        (what the PR gate runs)
make ci                  # tidy + verify + build + attest
make ci STRATEGY=docker  # force the Docker builder (FIRESTREAM_BUILD_STRATEGY)
make test-util           # src/util workspace; depends on both parity gates
```

## Development Tips

### DNS Resolution Testing
Test K8s internal DNS from devcontainer:
```bash
ping service_name.namespace.svc.cluster.local
```

### Resource Management
The devcontainer requires minimum:
- 4 CPUs
- 8GB memory
- 32GB storage

### GPU Support
If NVIDIA GPU detected, the system automatically installs container toolkit via `bin/host_scripts/nvidia-debian.sh`

### Port Forwarding
Default forwarded ports in devcontainer: 3000
Services are port-forwarded to localhost after deployment.

### Python Interpreter Path
In devcontainer: `/home/firestream/.python` (symlinked from Nix environment)

## Common Gotchas

1. **Running Outside Container**: `bootstrap.sh` detects if running outside Docker and auto-launches via `docker compose`
2. **Rust Analyzer**: Uses `--target-dir=target-ra` to avoid conflicts with main build
3. **CPU Architecture**: Build scripts auto-detect and adapt to x86_64 or ARM64
4. **Helm Charts**: Use the local fork in `src/charts/`, not upstream Bitnami. Chart deploys go through the flake-emitted bundle at `/opt/firestream/charts/` (see "Helm Chart Contract" above), not directly via `helm install` against `src/charts/firestream/<name>/`.
5. **Workspace Default Member**: `cargo run` at root runs the TUI, not the API server
6. **`helm-manager` is not a chart store**: it is a helm/kubectl CLI wrapper. Callers must pass `values.yaml` explicitly via `values_files` (no defaults sourced from embedded charts).
