# Example Odoo extension uv2nix workspace

This is a copy-and-adapt fixture for the `pythonWorkspace` seam on Firestream's
python-workspace containers, shown here for Odoo. It declares Python packages
that your Odoo addons import **in-process**, as **your own** uv2nix workspace.
Firestream merges it into the Odoo image's **primary** venv at build time.

Compare `src/templates/airflow_dags_workspace/`, which uses `extraWorkspaces` to
build a **separate** venv. That shape suits out-of-process guests such as DAG
tasks. It does not suit Odoo: the Odoo process imports addon dependencies
itself, so only the primary venv counts.

## Why it lives here (not under the odoo container)

`src/containers/firestream/odoo/18/` **is** the Odoo 18 uv2nix `workspaceRoot`.
A nested `pyproject.toml` inside it would confuse `loadWorkspace`, so this
fixture lives under `src/templates/`, which nothing scans as a container or
workspace.

## Layout

```
odoo_python_workspace/
├── pyproject.toml          # [project] + deps; NOT named "firestream-odoo"
├── uv.lock                 # committed lockfile (REQUIRED by uv2nix)
├── overrides.nix           # pycairo sdist build inputs; freetype-py runtime libs
└── src/firestream_odoo_extra_deps/__init__.py   # hatchling needs a package
```

## Two modes

- **extend** (this fixture). A small workspace with only the extra packages.
  Firestream composes its package overlay underneath the base one and builds
  **one** venv from both roots. Best when you add packages and keep Firestream's
  pins.
- **replace**. Your workspace becomes the primary root. Start from
  `src/containers/firestream/odoo/18/pyproject.toml`, add your packages, run
  `uv lock`, and set `replace.src`. Best when you must change a pin Firestream
  owns. Firestream's `overrides.nix` still composes underneath yours unless you
  set `inheritOverrides = false`.

## The three guards

1. **requires-python** must admit the container's interpreter (`>=3.12,<3.13`
   for Odoo 18). The factory throws a legible error otherwise.
2. **Lock compatibility** (extend only). Firestream diffs your `uv.lock` against
   the base lock in Nix. Any shared package at a different version, and any
   member-name collision, is a hard error that names the packages and both
   versions. Fix it by pinning to the base version, by dropping the package, or
   by switching to `replace`.
3. **Member name.** Your `[project].name` must not be `firestream-odoo`.

The pinned uv2nix does not read `[tool.uv.extra-build-dependencies]`. An
sdist-only package this workspace introduces gets its build backend in
`overrides.nix` through `final.resolveBuildSystem` (see the `pycairo` entry).
Shared packages keep Firestream's build config because the base overlay is
composed last.

## Freshness contract (user responsibility)

uv2nix builds from the **committed `uv.lock`**. After editing `pyproject.toml`:

```bash
uv lock --python 3.12   # regenerate
uv lock --check         # freshness gate: fails if pyproject.toml and uv.lock disagree
```

## Wiring it up

```nix
# passed as the last module to firestream.lib.<sys>.images.odoo-18.eval
{ ... }: {
  config.odoo.pythonWorkspace.extend = [
    { src = ./path/to/this/workspace; }   # overrides.nix auto-loaded from src
  ];
}
```

Verify inside the built image:

```bash
docker run --rm <image> python -c "import cairo, rlPyCairo, freetype, plaid; print('ok')"
```
