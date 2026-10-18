# Convenience targets for the Firestream Cloudflare-edge GKE example.
# Project-specific values come from config.nix (single source of truth).

NAMESPACE := $(shell nix eval --raw --impure --expr '(import ./config.nix).namespace')
HOSTNAME := $(shell nix eval --raw --impure --expr '(import ./config.nix).hostname')

.PHONY: help charts render pulumi-preview pulumi-up images deploy logs status destroy clean

help:
	@echo "Targets:"
	@echo "  charts          Build all three chart bundles"
	@echo "  render          Print the rendered manifests for all three"
	@echo "  pulumi-preview  Preview the infrastructure (cluster, registry, tunnel, DNS)"
	@echo "  pulumi-up       Create it"
	@echo "  deploy          Build + push images, sync secrets, deploy all three charts"
	@echo "  logs            Tail the cloudflared connector's logs"
	@echo "  status          Show pods / services / PVCs in the '$(NAMESPACE)' namespace"
	@echo "  destroy         Tear down the Pulumi stack (cluster, tunnel, DNS record)"
	@echo "  clean           Remove local nix result symlinks"

charts:
	nix build .#odoo-chart --out-link result-odoo-chart
	nix build .#nginx-chart --out-link result-nginx-chart
	nix build .#cloudflared-chart --out-link result-cloudflared-chart

render: charts
	@for c in odoo nginx cloudflared; do \
	  echo "# ================= $$c ================="; \
	  cat result-$$c-chart/rendered.yaml; \
	done

pulumi-preview:
	cd pulumi && pulumi preview

pulumi-up:
	cd pulumi && pulumi up

images deploy:
	bash scripts/deploy-local.sh

logs:
	kubectl -n $(NAMESPACE) logs -l app.kubernetes.io/name=cloudflared --tail=50 -f

status:
	kubectl -n $(NAMESPACE) get pods,svc,pvc

destroy:
	cd pulumi && pulumi destroy

clean:
	rm -f result result-*
