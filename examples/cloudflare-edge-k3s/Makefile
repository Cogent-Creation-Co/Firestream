# Convenience targets for the Firestream Cloudflare-edge local-k3s example.
# Project-specific values come from config.nix (single source of truth).

NS := $(shell nix eval --raw --impure --expr '(import ./config.nix).namespace')
HOST := $(shell nix eval --raw --impure --expr '(import ./config.nix).hostname')

.PHONY: help charts render deploy smoke logs status destroy clean

help:
	@echo "Targets:"
	@echo "  charts   Build all three chart bundles (odoo, nginx, cloudflared)"
	@echo "  render   Print the rendered manifests for all three"
	@echo "  deploy   Build + side-load images, create the token Secret, deploy all three"
	@echo "  smoke    Prove the edge routes (nginx -> odoo, 8069 and 8072)"
	@echo "  logs     Tail the cloudflared connector's logs"
	@echo "  status   Show pods / services / PVCs in the '$(NS)' namespace"
	@echo "  destroy  Delete the '$(NS)' namespace (all three releases + PVCs)"
	@echo "  clean    Remove local nix result symlinks"

# --- Charts (no cluster needed) ---------------------------------------------
charts:
	nix build .#odoo-chart --out-link result-odoo-chart
	nix build .#nginx-chart --out-link result-nginx-chart
	nix build .#cloudflared-chart --out-link result-cloudflared-chart

render: charts
	@for c in odoo nginx cloudflared; do \
	  echo "# ================= $$c ================="; \
	  cat result-$$c-chart/rendered.yaml; \
	done

# --- Deploy -----------------------------------------------------------------
deploy:
	bash scripts/deploy-local.sh

smoke:
	bash scripts/smoke.sh

logs:
	kubectl -n $(NS) logs -l app.kubernetes.io/name=cloudflared --tail=50 -f

status:
	kubectl -n $(NS) get pods,svc,pvc

# --- Teardown ---------------------------------------------------------------
destroy:
	kubectl delete namespace $(NS)

clean:
	rm -f result result-*
