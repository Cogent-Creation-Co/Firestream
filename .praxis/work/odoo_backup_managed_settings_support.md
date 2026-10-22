---
id: FIRE-007
title: "Odoo backup + managed settings support"
type: feature
status: done
priority: medium
tags: [add]
affects: []
depends-on: []
---

# Odoo backup + managed settings support

Minted by `praxis commit` to carry changes that no ticket claimed.

## Files (12)

- `src/charts/firestream/odoo/nix/default.nix`
- `src/charts/firestream/odoo/nix/options/app.nix`
- `src/charts/firestream/odoo/nix/options/backup.nix`
- `src/charts/firestream/odoo/nix/options/managed-settings.nix`
- `src/charts/firestream/odoo/templates/deployment.yaml`
- `src/charts/firestream/odoo/values.yaml`
- `src/containers/firestream/odoo/module.nix`
- `src/containers/firestream/odoo/options.nix`
- `src/containers/firestream/odoo/scripts/init.sh`
- `src/containers/firestream/odoo/scripts/odoo_backup.py`
- `src/containers/firestream/odoo/scripts/odoo_tools.py`
- `src/containers/firestream/odoo/tests/test_odoo_backup.py`
