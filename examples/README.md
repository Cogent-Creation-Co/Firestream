# Firestream Deployment Examples

This directory holds **reference deployments** — self-contained patterns for
taking a Firestream-supported app, layering a small override to "make it your
own," and shipping it to a real environment.

Each example is a standalone git-repo pattern you can copy out wholesale: a root
`flake.nix` that imports Firestream as an input, plus whatever cloud plumbing
(IaC, CI) that target needs.

## The pattern

A "Firestream deployment" is, at its core, **one flake**:

```nix
# flake.nix
inputs.firestream.url = "github:Cogent-Creation-Co/Firestream-one-flake-to-rule-them-all";

# ...then re-evaluate a supported app with YOUR overrides deep-merged on top:
packages.odoo-chart =
  (firestream.lib.${system}.charts.odoo.eval (import ./firestream/odoo-overrides.nix))
    .chartBundle;
```

`charts.<app>.eval` is the standardized customization seam
(`docs/firestream-supported-app.md` §7). It re-runs the chart's typed-option
evaluation with your module added last, so you override a single value without
re-stating the whole binding. It returns a `chartBundle` derivation containing
the forked chart, a Nix-emitted `values.yaml`, a `chart-manifest.json`, and an
executable `bin/deploy` (`helm upgrade --install …`).

Your override module is a **strict subset** of the chart's value surface (the
same keys as `src/charts/firestream/<app>/values.yaml`). You never fork chart
templates and never restate Firestream's image wiring — you only set what's
specific to you (credentials, storage class, ingress hostname, image registry).

The result is deployable three ways, all equivalent:

- `nix run .#deploy -- --namespace <ns>` (or `./result/bin/deploy …`)
- `helm upgrade --install <rel> result/chart -f result/values.yaml`
- `firestream helm deploy <app>` (when the bundle is published into a
  `FIRESTREAM_CHARTS_DIR` aggregate with an `index.json`)

## Examples

Each app ships as a trio — the *same app* via the *same* `charts.<app>.eval` /
`images.<app>.eval` seams, aimed at three runtimes (production cloud, a local
cluster, plain Docker). The `*-gke` variant is the production shape; the `*-k3s`
and `*-docker-compose` variants are its local-dev shapes.

The exception is the **Cloudflare edge** example at the bottom, which is about
composing several charts rather than about one app across three runtimes.

**Odoo** — customization: vendored third-party addons baked into the image.

| Example | Target | Highlights |
|---------|--------|------------|
| [`odoo-gke/`](./odoo-gke) | GKE Autopilot | Pulumi cluster + Artifact Registry + Secret Manager; Cloud Build deploy; credentials via `existingSecret` |
| [`odoo-k3s/`](./odoo-k3s) | Local k3s / k3d | Build → side-load images into containerd → helm → port-forward; inline credentials; `local-path` storage |
| [`odoo-docker-compose/`](./odoo-docker-compose) | Docker Compose | No Kubernetes; reuses Firestream's generated compose stack; custom image with vendored addons |

**Airflow** — customization: a **custom DAG baked into the image** (the Airflow
analogue of odoo's vendored addons, via `config.airflow.vendoredDags`).
CeleryExecutor + bundled PostgreSQL & Redis.

| Example | Target | Highlights |
|---------|--------|------------|
| [`airflow-gke/`](./airflow-gke) | GKE Autopilot | Baked DAG; 3 images → Artifact Registry + Secret Manager; Cloud Build; `existingSecret` |
| [`airflow-k3s/`](./airflow-k3s) | Local k3s / k3d | Baked DAG; side-load 3 images into containerd → helm → port-forward; inline credentials; `local-path` |
| [`airflow-docker-compose/`](./airflow-docker-compose) | Docker Compose | Baked DAG; reuses Firestream's generated multi-service compose stack |

A separate Airflow example demonstrates a *different* customization —
**guest DAG dependencies as a separate uv2nix venv** (`config.airflow.dagWorkspace`).
Where the trio above bakes a *DAG* into the image, this bakes the *Python
dependencies* your DAGs need into a **separate, isolated** venv at
`/opt/firestream/airflow/dags-venv` (Airflow's own closure untouched), reached
from DAG code via the baked `FIRESTREAM_DAGS_VENV` / `FIRESTREAM_DAGS_PYTHON`
env vars.

| Example | Target | Highlights |
|---------|--------|------------|
| [`airflow-dag-dependencies/`](./airflow-dag-dependencies) | Local k3s / k3d | Guest deps as a `dags-workspace/` uv2nix workspace (committed `uv.lock`) → separate venv via `config.airflow.dagWorkspace`; both consumption shapes (BashOperator → console-script, `@task.external_python`); side-load 3 images; inline credentials; `local-path` |

**Next.js** — a **net-new (non-Bitnami) supported app**: a production Next.js
server built to standalone output by the Firestream Node factory
(`mkNodePackage` + `mkNodeContainerModule`), with your app baked into the image
via `config.nextjs.vendoredApp` and a **bundled PostgreSQL** subchart running
the firestream-postgresql image. The homepage runs a live `SELECT` against the
database.

| Example | Target | Highlights |
|---------|--------|------------|
| [`nextjs-gke/`](./nextjs-gke) | GKE Autopilot | Custom app; 2 images → Artifact Registry + Secret Manager; Cloud Build; GCE ingress; `existingSecret` |
| [`nextjs-k3s/`](./nextjs-k3s) | Local k3s / k3d | Custom app; side-load 2 images into containerd → helm → port-forward; inline credentials; `local-path` |
| [`nextjs-docker-compose/`](./nextjs-docker-compose) | Docker Compose | Custom app; reuses Firestream's generated nextjs + postgresql compose stack |

**Cloudflare edge** — customization: *composition*. The first example that
deploys more than one chart.

Odoo behind the `nginx` reverse proxy behind the `cloudflared` tunnel connector,
three releases in one namespace. There is no `Ingress`, no LoadBalancer and no
public port — the connector dials *out* to Cloudflare and traffic arrives back
through the tunnel, which is why the cloudflared chart renders no Service at
all.

| Example | Target | Highlights |
|---------|--------|------------|
| [`cloudflare-edge-k3s/`](./cloudflare-edge-k3s) | Local k3s / k3d | 3 charts, 1 namespace; `upstreams` routing with Odoo's 8069/8072 split; fake tunnel token so the connector is exercised without a Cloudflare account; differential smoke probes |

Two things it teaches that a single-chart example cannot:

- **Charts that must agree.** nginx's `upstreams` names Odoo's Service and both
  its ports, and Odoo must be running in the mode that actually binds them
  (`workers > 0`). Nothing type-checks that agreement across charts, so
  `config.nix` holds the shared facts once.
- **Deploy order is a property of the values, not a fixed rule.** With
  `nginxConfig.resolver` unset, nginx resolves upstream names at config load and
  refuses to start if a Service is missing — so the backend must be deployed
  first. Setting the resolver makes the proxy boot regardless and answer 502
  until the backend appears, which downgrades the ordering from a requirement to
  a convenience.

Future examples follow the same shape — a root flake + a sparse override + the
target's plumbing. Start a new one by copying the closest existing example and
swapping the app name in the `charts.<app>.eval` call. For a multi-chart
example, copy `cloudflare-edge-k3s/`: one `charts.<app>.eval` per chart in the
flake, one override file each, and a deploy script that sequences them.
