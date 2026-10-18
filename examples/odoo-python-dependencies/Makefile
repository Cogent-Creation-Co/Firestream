# Convenience targets for the Firestream Odoo + Python dependencies example.
# Project-specific values come from config.nix (single source of truth).

NS := $(shell nix eval --raw --impure --expr '(import ./config.nix).namespace')

.PHONY: help chart render lock lock-check image deploy verify port-forward status destroy clean

help:
	@echo "Targets:"
	@echo "  chart        Build the customized Odoo chart bundle (.#odoo-chart)"
	@echo "  render       Print the rendered Kubernetes manifests"
	@echo "  lock         Regenerate python-workspace/uv.lock after editing pyproject.toml"
	@echo "  lock-check   Fail if python-workspace/uv.lock is stale"
	@echo "  image        Build the custom firestream-odoo:18.0 image (extra deps merged in)"
	@echo "  deploy       Build + side-load images, then helm upgrade --install (scripts/deploy-local.sh)"
	@echo "  verify       Import the extra packages inside the running pod and show the odoo.conf limits"
	@echo "  port-forward Forward localhost:8069 -> svc/odoo (Ctrl-C to stop)"
	@echo "  status       Show pods / services / PVCs in the '$(NS)' namespace"
	@echo "  destroy      Delete the '$(NS)' namespace (release + PVCs) for a clean reinstall"
	@echo "  clean        Remove local nix result symlinks"

# --- Chart (no cluster needed) ---------------------------------------------
chart:
	nix build .#odoo-chart --out-link result

render: chart
	cat result/rendered.yaml

# --- Python workspace ------------------------------------------------------
lock:
	cd python-workspace && uv lock --python 3.12

lock-check:
	cd python-workspace && uv lock --check

image: lock-check
	nix build .#odoo-image --out-link result-odoo-image

# --- Deploy ----------------------------------------------------------------
deploy: lock-check
	bash scripts/deploy-local.sh

# The venv is on PATH inside the image, so a bare `python` is the merged venv.
verify:
	kubectl -n $(NS) exec deploy/odoo -- python -c 'import cairo, rlPyCairo, freetype, plaid; print("extra deps ok:", cairo.version, freetype.version())'
	kubectl -n $(NS) exec deploy/odoo -- grep -E '^(workers|limit_|max_cron_threads|list_db)' /opt/firestream/odoo/conf/odoo.conf

port-forward:
	kubectl -n $(NS) port-forward svc/odoo 8069:80

status:
	kubectl -n $(NS) get pods,svc,pvc

# --- Teardown --------------------------------------------------------------
destroy:
	kubectl delete namespace $(NS)

clean:
	rm -f result result-*
