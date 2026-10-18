# Odoo with extra in-process Python dependencies — a Firestream deployment

A complete, copyable pattern for running the Firestream **Odoo** app on a
**local Kubernetes cluster** (host k3s or a throwaway k3d) with two
customizations the stock trio does not show:

- **Extra Python packages your addons import in-process**, declared as your own
  uv2nix workspace (`python-workspace/`) and **merged into the Odoo image's own
  venv** via `config.odoo.pythonWorkspace.extend`. No fork of the image.
- **Production-shaped `odoo.conf` limits** (`workers`, `limit_memory_*`,
  `limit_time_*`, `limit_request`, `max_cron_threads`, `list_db`) set as typed
  chart options from `config.nix`.

Everything else is the local-dev shape of [`../odoo-k3s`](../odoo-k3s):
bundled PostgreSQL, inline credentials, `local-path` storage, ClusterIP +
`kubectl port-forward`, and `firestream-*` images side-loaded into containerd.

## Why not a separate venv?

Firestream's Airflow example ([`../airflow-dag-dependencies`](../airflow-dag-dependencies))
bakes guest dependencies into a **separate** venv. That works for Airflow
because a DAG task can run a different interpreter across a process boundary.
Odoo cannot: the Odoo server process imports addon dependencies itself. So the
packages must land in the **primary** venv, and that is what `pythonWorkspace`
does. `extraWorkspaces` / `dagWorkspace` are the wrong tool here.

## Layout

```
odoo-python-dependencies/
├── flake.nix                      # imports firestream; packages.{odoo-chart,odoo-image,postgresql-image}
├── config.nix                     # ← YOU EDIT THIS: namespace, passwords, odoo.conf limits
├── python-workspace/              # ← YOUR uv2nix workspace of extra in-process deps
│   ├── pyproject.toml             #    pycairo, rlPyCairo, freetype-py (NOT named firestream-odoo)
│   ├── uv.lock                    #    committed; uv2nix builds from it
│   ├── overrides.nix              #    pycairo sdist build inputs (meson-python, cairo)
│   └── src/odoo_addons_deps/      #    hatchling needs a package dir
├── firestream/
│   ├── odoo-image-overrides.nix   # config.odoo.pythonWorkspace.extend = [ { src = ../python-workspace; } ]
│   └── odoo-overrides.nix         # sparse CHART override + the odoo.conf limits
├── scripts/deploy-local.sh        # build → side-load images → helm upgrade --install
└── Makefile                       # chart / lock / lock-check / image / deploy / verify / …
```

## The contract, in one paragraph

Firestream loads your workspace with uv2nix, composes its package overlay
**underneath** the Odoo 18 workspace's overlay, and builds **one** venv from
both roots. Before it does, it diffs the two `uv.lock` files in Nix. Any package
present in both at **different versions**, and any workspace member named
`firestream-odoo`, is a hard error that names the offenders:

```
python-workspace: extension /nix/store/…-python-workspace is not lock-compatible with base /nix/store/…-18
  version conflicts (package: base=X ext=Y):
      plaid-python: base=43.0.0 ext=44.0.0
Fix: pin the extension to the base version (uv add '<name>==<base>' && uv lock),
drop the package from the extension (it is already in the base lock), or use
pythonWorkspace.replace to own the whole lock.
```

That is why this workspace does **not** list `plaid-python`: Firestream's lock
already pins it, and your addons import it from there. Shared packages keep
Firestream's build configuration; your workspace only contributes **new**
names. `requires-python` must admit 3.12 (Odoo 18's interpreter).

## Quickstart

### 0. Prerequisites
`nix` (flakes enabled), `docker`, `kubectl`, `helm`, `uv`, and **one** of a host
k3s (you are in the `k3s` group) or a k3d cluster (`k3d cluster create odoo`,
then `export K3D_CLUSTER=odoo`). `nix develop` (or `direnv allow`) provides the
tools.

### 1. Edit `python-workspace/pyproject.toml`
Add the packages your addons import, then:
```bash
make lock          # uv lock --python 3.12
make lock-check    # fails if pyproject.toml and uv.lock disagree
```
An sdist-only package that needs a build backend or system headers gets an
entry in `python-workspace/overrides.nix` (see the `pycairo` one).

### 2. Build the image and prove the merge (no cluster needed)
```bash
make image
docker load < result-odoo-image
docker run --rm firestream-odoo:18.0 python -c "import cairo, rlPyCairo, freetype, plaid; print('ok')"
```

### 3. Deploy and verify
```bash
make deploy        # lock-check, build images, side-load them, helm upgrade --install
make status        # wait until both pods are 1/1 Running
make verify        # imports the extra packages in the pod; prints the odoo.conf limits
make port-forward  # localhost:8069 -> svc/odoo
```
Log in with `user@example.com` / `admin1234` (from `config.nix`).

## Replacing the lock instead of extending it

Use `replace` when you must **change a pin Firestream owns** (for example a
different `plaid-python`):

```bash
cp <firestream>/src/containers/firestream/odoo/18/pyproject.toml python-workspace-full/
# edit dependencies, then
(cd python-workspace-full && uv lock --python 3.12)
```

```nix
config.odoo.pythonWorkspace.replace = {
  src = ../python-workspace-full;
  # inheritOverrides = true;  # keep Firestream's system-library build inputs underneath yours
};
```

`module.nix` still comes from Firestream; only the workspace root changes.

## The odoo.conf limits

`config.nix` holds them once; `firestream/odoo-overrides.nix` passes them to the
chart, which emits each as an `ODOO_*` env var **only when set**. The container
renders `odoo.conf` from those on every pod start (the conf dir lives on the
rootfs, not the PVC), so a changed value lands on the next rollout. Unset knobs
leave the container's baked defaults, which equal Odoo's own defaults.

| Chart option | odoo.conf key | Note |
|---|---|---|
| `workers` | `workers` | `> 0` = prefork mode; required for per-worker memory limits and for the gevent port 8072 |
| `limitMemorySoft` / `limitMemoryHard` | `limit_memory_soft` / `_hard` | bytes per worker; size with the pod memory request and `workers` |
| `limitTimeCpu` / `limitTimeReal` / `limitTimeRealCron` | `limit_time_*` | seconds; `limitTimeRealCron = -1` means "same as real" |
| `limitRequest` | `limit_request` | requests per worker before recycling |
| `maxCronThreads` | `max_cron_threads` | `0` disables cron in this pod |
| `listDb` | `list_db` | keep `false` in production |

Docker Compose users with a persisted conf directory must also set
`ODOO_FORCE_OVERWRITE_CONF=yes` for a changed value to be re-rendered.

## Notes & gotchas

- **`/bitnami/python/requirements.txt`** is a legacy, non-functional path: the
  venv is a read-only Nix store path, so a runtime `pip install` cannot add
  packages. `pythonWorkspace` is the supported route.
- **Separate image stores.** Rebuilding and `docker load`-ing does not change
  what the cluster runs until it is re-imported; `make deploy` does that on
  every run.
- **In-repo testing (before this lands on `main`).** The flake pins the public
  github input. To test against your local checkout:
  ```bash
  NIX_OVERRIDE="--override-input firestream path:../.." make deploy
  ```
