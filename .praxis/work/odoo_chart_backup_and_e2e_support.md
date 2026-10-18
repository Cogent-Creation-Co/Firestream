---
id: FIRE-005
title: "Odoo chart, backup, and e2e support"
type: feature
status: done
priority: medium
tags: [add]
affects: []
depends-on: []
---

# Odoo chart, backup, and e2e support

Minted by `praxis commit` to carry changes that no ticket claimed.

## Files (24)

- `src/charts/firestream/odoo/nix/default.nix`
- `src/charts/firestream/odoo/nix/options/app.nix`
- `src/charts/firestream/odoo/nix/options/backup.nix`
- `src/charts/firestream/odoo/templates/deployment.yaml`
- `src/charts/firestream/odoo/values.yaml`
- `src/containers/firestream/odoo/18/overrides.nix`
- `src/containers/firestream/odoo/18/pyproject.toml`
- `src/containers/firestream/odoo/18/uv.lock`
- `src/containers/firestream/odoo/module.nix`
- `src/containers/firestream/odoo/options.nix`
- `src/containers/firestream/odoo/scripts/config.sh`
- `src/containers/firestream/odoo/scripts/helpers.sh`
- `src/lib/rust/firestream-charts/src/spec.rs`
- `src/lib/rust/firestream-e2e-core/src/k8s/images.rs`
- `src/lib/rust/firestream-e2e-k8s/src/lib.rs`
- `src/lib/rust/firestream-e2e-k8s/src/odoo_backup.rs`
- `src/lib/rust/firestream-e2e-k8s/tests/e2e_k8s.rs`
- `src/lib/rust/firestream/src/cli/commands.rs`
- `src/lib/rust/firestream/src/deploy/helm_lifecycle/from_manifest.rs`
- `src/lib/rust/helm-manager/src/kubectl_client.rs`
- `src/templates/odoo_python_workspace/overrides.nix`
- `src/templates/odoo_python_workspace/pyproject.toml`
- `src/templates/odoo_python_workspace/src/firestream_odoo_extra_deps/__init__.py`
- `src/templates/odoo_python_workspace/uv.lock`
