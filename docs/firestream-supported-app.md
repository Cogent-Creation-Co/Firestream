# Firestream Supported App — Architecture Specification

> Status: authoritative architecture spec.
> Audience: anyone adding, maintaining, or consuming a Firestream application.
> Related: [`app-philosophy.md`](./app-philosophy.md).

---

## 0. Why Firestream

**Firestream is a fork and rewrite of Bitnami. It depends on Bitnami for nothing.**

- No upstream tracking. We do not follow Bitnami releases.
- No Bitnami packages, no Bitnami binaries, no Bitnami container images.
- No Bitnami proprietary build tooling anywhere in the build graph.

What Firestream keeps is the *shape* of the Bitnami Helm charts. Those charts are **forked
into this repository and vendored under `src/charts/firestream/<app>/`, modified to
Firestream's needs.** They are the source of truth for Kubernetes templating.

The mission this document codifies:

> **Replace every proprietary Bitnami build tool with Nix.** Firestream's containers are
> clean-room, reproducible Nix builds that drop into the forked charts in place of Bitnami's
> closed image pipeline.

Everything below follows from that mission. In particular it sets a hard boundary on *what
Nix is responsible for* versus *what the forked chart owns* — a boundary that has been
violated before (by attempting to regenerate chart templates from Nix) and must not be again.

### The guiding principle

Nix holds **only** the small set of things needed to:

1. **Build the container** — the part that replaces Bitnami's proprietary build.
2. **Compute a `values.yaml` overlay** — the small set of Firestream-specific values that adapt
   the forked chart to the Nix-built container.
3. **Emit a `docker-compose.yml`** — the same image/port model as the chart, for local use.

Nix does **not** reimplement chart templating. The forked-and-vendored chart YAML stays the
source of truth for templates. Render is always:

```
helm template <forked-chart> -f <nix-emitted values.yaml>
```

---

## 1. Definition

A **Firestream Supported App** is a single application definition in the central flake that
emits four coordinated artifacts:

| # | Artifact | What it is |
|---|----------|-----------|
| 1 | **Nix-built container image** | The Bitnami-build replacement: a reproducible image built entirely by Nix. |
| 2 | **`values.yaml` overlay + `chart-manifest.json`** | The small Firestream-specific value overlay applied to the forked chart, plus deployment metadata. |
| 3 | **Forked-and-vendored Helm chart** | The chart YAML, forked from Bitnami and modified to our needs. Templates live here. |
| 4 | **`docker-compose.yml`** | A local-dev stack using the same image + ports as the chart. |

The boundary, stated once: **Nix holds and builds artifacts 1, 2, and 4; the chart YAML
(artifact 3) is hand-maintained in `src/charts/` and is never regenerated from Nix.**

### Chart-only apps

A **chart-only app** emits artifacts 2 and 3 but not 1 or 4, because there is no image for
Nix to build: the app runs a third party's image. `cloudflared` is the case — Cloudflare's
connector binary, shipped as `cloudflare/cloudflared`, with no
`src/containers/firestream/cloudflared/` and no `firestreamImages.cloudflared`. No
`docker-compose.yml` follows either, since `compose.nix` iterates `firestreamImages`; that is
correct rather than a gap, as a tunnel connector has nothing to stand up locally.

The image-injection machinery needs no special case for this. `_meta.containerRefs` is a
plain declarative attrset, and `inject-container-images.nix` already defines
`componentPath = [ ]` as *catalogue-only: record in `chart-manifest.json`, contribute no
values overlay*. So the upstream triple is registered as a catalogue entry — the manifest
still documents exactly what image runs, for the deploy layer and any SBOM tooling — while
the chart's own `values.yaml` stays the single source of truth for the pin. Because that
duplicates the triple across two files, the app's render-fidelity check must assert the two
agree (see `cloudflared-render-fidelity` in `nix/flake-modules/charts/checks.nix`).

Two further consequences: `global.security.allowInsecureImages` is not set (that flag exists
to get Firestream's own non-Bitnami images past the `common` helpers' whitelist; an upstream
image needs no bypass), and the generated `values.yaml` is empty with no consumer overrides —
the strongest possible form of "the overlay is a strict subset of the chart's value surface".

---

## 2. The four outputs (per app)

Every supported app `<app>` contributes the following flake attributes. The 8 canonical apps
are: `airflow`, `postgresql`, `redis`, `kafka`, `spark`, `jupyterhub`, `superset`, `odoo`.
`seaweedfs`, `nextjs` and `nginx` follow the same shape from non-Bitnami chart forks;
`cloudflared` is the chart-only app described in §1.

| Output | Flake attribute(s) | Produced from |
|--------|--------------------|---------------|
| Container image | `packages.<app>`, `firestreamImages.<app>` | `src/containers/firestream/<app>/module.nix` via `mkPythonContainerModule` / `mkContainerModule` (`bin/nix/firestream/containers/base.nix`, `apps/base.nix`) |
| Overlaid chart bundle (`values.yaml` + `chart-manifest.json`) | `packages.<app>-chart`, `firestreamCharts.<app>`, `charts.<app>.chartBundle` | Typed-option overlay through `bin/nix/firestream/charts/eval-chart.nix` |
| Base chart (native default values) | `packages.<app>-base-chart`, `charts.<app>.baseChart` | `bin/nix/firestream/charts/lib/base-chart.nix` — the forked chart with its own `values.yaml`, no overlay; `$out` is the chart dir so `helm install <rel> <storepath>` works directly |
| Forked chart (templates) | *(source, not an output)* `src/charts/firestream/<app>/` | Forked from Bitnami, modified, vendored via `bin/nix/firestream/charts/lib/vendor-subcharts.nix` |
| `docker-compose.yml` | `packages.<app>-compose`, `apps.<app>-up`, `apps.<app>-down` | `nix/flake-modules/compose.nix` |

The chart is exposed in **two forms**, both importable by downstream consumers: the
**overlaid bundle** (`chartBundle` — Firestream image injection + path remaps + manifest) and the
**base chart** (`baseChart` — the plain forked chart with Bitnami's native defaults). The aggregate
`index.json` catalogues both (`charts` and `baseCharts`).

All four build on every platform (Darwin included): only the Docker *image build* is
Linux-gated; reading a container's evaluated config (image ref, ports, paths) to drive the
chart overlay and compose file does not force the image build.

---

## 3. What Nix holds vs. what the forked chart owns

This is the core principle, made concrete.

**Nix holds (and builds from scratch, replacing Bitnami's proprietary tooling):**

- The container source: `pyproject.toml`/`uv.lock`, `module.nix`, `options.nix`, `scripts/`.
- A **typed-option overlay** — the override contract (§4).
- Image references (registry / repository / tag) for the chart's image slots.
- **Path remaps** that reconcile container FHS paths with the chart's mount layout (§5b).
- Helper script text mounted into the container (§5c).
- A handful of Firestream-specific value toggles.

**The forked chart owns (and Nix never regenerates):**

- `Chart.yaml`, `values.yaml` (chart defaults), `templates/**`, `values.schema.json`.
- All Go-template / Helm templating logic.

The render is `helm template` over the forked chart with the Nix-emitted `values.yaml`. The
overlay is always a **strict subset** of the chart's existing value surface — if a behavior
needs a template change, that change is made in `src/charts/firestream/<app>/templates/`,
*not* synthesized in Nix.

---

## 4. The typed-option overlay contract

The override layer is a `lib.evalModules` schema, evaluated by
`bin/nix/firestream/charts/eval-chart.nix`. This is the standardized contract for *what a
Firestream app may override* on a chart.

- **Standard schema.** `eval-chart.nix` declares, under the chart's name, a `_meta` block
  (release name, namespace, `deployment.*` Helm flags, `lifecycle.*`, `containerRefs`,
  `provenance`) and a freeform `values` tree. See `eval-chart.nix:51-246`.
- **Per-chart options.** Each app's own option modules live at
  `src/charts/firestream/<app>/nix/` (entry `nix/default.nix`) and write typed values through
  to paths under `config.<app>.values.*`. Shared option types are at
  `bin/nix/firestream/charts/lib/types/` and are injected as `chartTypes` in `specialArgs`
  (`eval-chart.nix:255-260`) so per-chart modules don't count `../` hops.
- **Serialization.** `bin/nix/firestream/charts/lib/to-values-yaml.nix` null-strips the
  resolved `values` tree into a faithful block `values.yaml`.
- **Image injection.** `bin/nix/firestream/charts/lib/inject-container-images.nix` writes each
  `{ registry; repository; tag; }` triple at its slot's `componentPath` in the values tree
  before serialization; existing user values win at overlapping paths.
- **Build gate.** The bundle's build runs `helm template` fully offline; a chart that does not
  render fails the build (`eval-chart.nix:350-393`).

**Canonical example — `nix/flake-modules/charts/airflow.nix`:**

```nix
c = evalChart {
  name = "airflow";
  inherit chartSrc subcharts;          # chartSrc = src/charts/firestream/airflow
  modules = [ optionsPath imageInjectionModule ];
};
```

where `optionsPath = src/charts/firestream/airflow/nix/default.nix` and
`imageInjectionModule` populates `_meta.containerRefs` + the overlay (lines 117-137).

---

## 5. Container ↔ chart binding (the five bridges)

A Nix-built container must drop into the forked chart in place of the image the chart
originally expected. Because the chart is forked *from* Bitnami, it still carries Bitnami's
path and helper conventions (e.g. `/opt/bitnami/<app>/...`). The Nix container is built to
satisfy those conventions through five bridges. (Mirrors CLAUDE.md's "Helm Chart Contract".)

**(a) Image substitution.** Each chart flake-module sources the container's image triple by
re-evaluating the container with no overrides
(`config.firestreamImages.<app>.eval (_: {})`), writes it into `_meta.containerRefs.<slot>`
with the slot's `componentPath`, and sets
`global.security.allowInsecureImages = true` to bypass the chart's image-whitelist refusal
(the firestream image repository is not in the original whitelist). Ref:
`charts/airflow.nix:117-137`.

**(b) Path remap via `extraEnvVars`.** The container bakes its FHS under
`/opt/firestream/<app>/...` (and `/firestream/<app>/...`); the chart mounts writable
emptyDirs/PVCs at `/opt/bitnami/<app>/...`. The overlay injects `extraEnvVars` redirecting
every runtime-writable `*_DIR` / `*_FILE` var to a path the chart actually mounts. On
multi-pod charts a single top-level `extraEnvVars` covers all components. Ref:
`charts/airflow.nix:107-115`.

**(c) Helper visibility.** Chart init containers `source /opt/bitnami/scripts/lib<app>.sh`
then call helpers (`<app>_conf_set`, etc.). The container build emits those helpers at the top
level of `/opt/firestream/scripts/libhelpers<app>.sh` (via `perContainerHelpers` on
`mkAppModule`) and symlinks `/opt/bitnami/scripts -> /opt/firestream/scripts`
(`bin/nix/firestream/containers/base.nix`) so the chart's `source` lines resolve unchanged.

**(d) Env-default semantics.** `bin/nix/firestream/env/defaults.nix` emits
`export VAR="${VAR:-default}"` so a K8s-injected env value always wins over the
container-baked default — without this, the chart's `extraEnvVars` remaps would be silently
overwritten.

**(e) State-dir tolerance.** Chart pods often run `readOnlyRootFilesystem: true` with no PVC at
`/firestream`. The shared persistence/state helpers
(`bin/nix/firestream/lib/{persistence,state}.nix`) silently skip on `EROFS`, so per-container
activation functions need no special-casing.

---

## 6. docker-compose parity

`nix/flake-modules/compose.nix` renders a `docker-compose.yml` for every app from the **same**
evaluated container module that drives the chart overlay, so the compose image tags always
match what the flake builds. It provides:

- `packages.<app>-compose` — derivation holding `docker-compose.yml` + `README.md`, with
  `passthru` = `{ composeFile; projectName; hostPorts; buildList; healthHostPort; }`.
- `apps.<app>-up` / `apps.<app>-down` — build+load the required images, then `docker compose
  up -d` / `down`.

Compose is a **first-class, co-equal output**, not an afterthought: same image, same ports
(with a host-port offset so multiple stacks coexist), healthchecks synthesized from the
container's `health.enable`, and `depends_on` ordering. This is what makes the flake usable
locally without Kubernetes.

---

## 7. Importability & overrides

The flake is importable by any downstream project, which then consumes the standardized,
Nix-shaped deployment and overrides only where needed.

```nix
# consumer flake.nix
inputs.firestream.url = "github:Cogent-Creation-Co/Firestream-...";
```

Per-app surface, exposed via `firestream.lib.<system>`:

- `images.<app>` → `{ imageRef; dockerImage; compose; buildApp; eval; options; }`
- `charts.<app>` → `{ chartBundle; render; eval; options; }`

The **`eval` hook is the standardized customization seam**: it re-evaluates the app with the
consumer's overrides deep-merged *on top of* the Firestream defaults (and, for charts, on top
of the image-injection overlay — see `charts/airflow.nix:155-161`), so consumers customize a
single value without re-stating the whole binding.

`images.<app>.eval` re-evaluates the **container** the same way `charts.<app>.eval`
re-evaluates the chart — the consumer supplies an options module and gets a custom
image's `dockerImage` back, without forking. A container option carries the
preference; the option's value is forwarded to the container factory through
`extraModuleArgs` (`eval-container.nix`). Example: Odoo's
`options.odoo.vendoredAddons` (`src/containers/firestream/odoo/options.nix`) takes
a list of addon-repo specs that `src/containers/firestream/odoo/vendor-addons.nix`
fetches and bakes into `/opt/odoo/vendor-addons` (wired into `addons_path`); a
downstream repo sets it via `images.odoo.eval` to get a custom image with its
addons baked in. See `examples/odoo/firestream/odoo-image-overrides.nix`.

Airflow's `options.airflow.dagWorkspace` (`src/containers/firestream/airflow/options.nix`)
is the same pattern for **guest-DAG Python dependencies**. It takes a directory that is the
consumer's **own uv2nix workspace** (`pyproject.toml` + `uv.lock`, plus an optional
`overrides.nix` auto-loaded from the dir). The generic `extraWorkspaces` seam on
`eval-container.nix` resolves it through uv2nix into a **separate** baked venv at
`/opt/firestream/airflow/dags-venv` — isolated from Airflow's own venv
(`/opt/firestream/airflow/venv`) and its resolution closure, yet first-class in downstream
Nix builds (the guest deps flow through the same wheel+sdist overlays, so they appear in the
source archive / SBOM). When set, two env vars are baked into every Airflow pod:
`FIRESTREAM_DAGS_VENV=/opt/firestream/airflow/dags-venv` and
`FIRESTREAM_DAGS_PYTHON=/opt/firestream/airflow/dags-venv/bin/python`. Because Airflow's
scheduler/dag-processor parse DAGs with Airflow's own interpreter, the guest venv is
deliberately kept off their `PYTHONPATH`; DAGs reach it across a process boundary via two
tiers: **(A)** a `BashOperator`/`@task.bash` running `$FIRESTREAM_DAGS_VENV/bin/<console-script>`
— separate process, guest venv needs nothing from Airflow (recommended; note BashOperator
pushes only the *last* stdout line to XCom); or **(B)** `@task.external_python(python=os.environ["FIRESTREAM_DAGS_PYTHON"])`
— Tier A ships no `apache-airflow` (serializable-in / value-out, no live context, genuinely
cheap), Tier B pins `apache-airflow==3.0.3` for full context/XCom fidelity at the cost of
re-coupling to Airflow's closure. `requires-python` must admit `python312`, or the factory
throws. Copy-and-adapt fixture: `src/templates/airflow_dags_workspace/`.

---

## 8. Authoring a new Supported App

To add `<app>`, create:

1. **Container** — `src/containers/firestream/<app>/`:
   - `module.nix` (calls `mkPythonContainerModule` / `mkContainerModule`): paths, env vars
     (with `_FILE` secret variants), `runtimeDirs`, system + runtime deps, exposed ports,
     `validateFn` / `initFn` / `configFn` / `activateFn` / `runCmd`, `perContainerHelpers`.
   - `options.nix` (typed container options), `scripts/` (`validate.sh`, `init.sh`, `config.sh`,
     `helpers.sh`, …), optional `docker-compose*.yml` for local dev.
   - `nix/flake-modules/containers/<app>.nix` — wires `evalContainer`; registers
     `packages.<app>` + `firestreamImages.<app>`.

2. **Chart** — `src/charts/firestream/<app>/`:
   - The forked Bitnami chart (`Chart.yaml`, `values.yaml`, `templates/**`), modified to needs.
   - `nix/default.nix` (+ `nix/options/`) — the typed-option overlay for this chart.
   - `nix/flake-modules/charts/<app>.nix` — wires `evalChart` with the options module + an
     image-injection module (image slots, `allowInsecureImages`, path remaps); registers
     `packages.<app>-chart`, `firestreamCharts.<app>`, `firestreamChartImages.<app>`.
     Use `charts/airflow.nix` as the template.

3. **Compose** — no per-app work: `compose.nix` iterates `firestreamImages` automatically.

---

## 9. Invariants (what makes an app "Supported")

- **All four outputs build** on every supported platform — or, for a chart-only app (§1), the
  two it declares. Which outputs an app emits is a property of the app, but emitting fewer
  than it declares is a failure.
- **The container is built entirely by Nix** — zero Bitnami build tooling or Bitnami binaries
  in the image closure. Vacuous for a chart-only app, which builds no container; in exchange
  it owes a check that its catalogue-only `containerRefs` entry matches the image its chart
  actually renders, since nothing else ties the two together.
- **The values overlay is a strict subset** of the forked chart's value surface — Nix never
  regenerates templates. Behavior changes that need a template change go into
  `src/charts/firestream/<app>/templates/`.
- **The chart renders offline** — `helm template` in the build sandbox is the gate.
- **Compose and chart agree** on image reference and ports (both derive from the same evaluated
  container module). A chart-only app has no compose file to agree with.
- **The app is importable** — `firestream.lib.<system>.{images,charts}.<app>` resolve and the
  `eval` override hook works.
