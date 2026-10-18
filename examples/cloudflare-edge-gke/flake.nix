{
  description = "Firestream Cloudflare edge — Odoo behind nginx behind a Cloudflare Tunnel, on GKE Autopilot";

  # ---------------------------------------------------------------------------
  # The production shape of ../cloudflare-edge-k3s: the same three charts through
  # the same `charts.<app>.eval` seams, but with a REAL Cloudflare tunnel.
  #
  #   Cloudflare edge --> cloudflared Deployment --> nginx Service :80 --> odoo
  #                       (outbound only)            (per-namespace proxy)
  #
  # There is no Ingress, no LoadBalancer and no public port. The only path into
  # the namespace is the tunnel, which the connector dials OUT to. See
  # ./pulumi/__main__.py for the tunnel, its ingress rules, its DNS record, and
  # the Secret carrying its token.
  # ---------------------------------------------------------------------------
  inputs = {
    firestream.url = "github:Cogent-Creation-Co/Firestream-one-flake-to-rule-them-all";

    nixpkgs.follows = "firestream/nixpkgs";
    flake-utils.follows = "firestream/flake-utils";
  };

  outputs = { self, firestream, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          config.allowUnfree = true; # google-cloud-sdk / pulumi
        };
        fs = firestream.lib.${system};

        odooChart = (fs.charts.odoo.eval
          (import ./firestream/odoo-overrides.nix)).chartBundle;

        nginxChart = (fs.charts.nginx.eval
          (import ./firestream/nginx-overrides.nix)).chartBundle;

        # cloudflared is a CHART-ONLY app — it runs Cloudflare's own connector
        # image, so there is no `fs.images.cloudflared` and nothing to push to
        # Artifact Registry. The kubelet pulls it from docker.io.
        cloudflaredChart = (fs.charts.cloudflared.eval
          (import ./firestream/cloudflared-overrides.nix)).chartBundle;
      in
      {
        packages = {
          default = nginxChart;

          odoo-chart = odooChart;
          nginx-chart = nginxChart;
          cloudflared-chart = cloudflaredChart;

          # Images pushed to Artifact Registry by scripts/deploy-local.sh.
          odoo-image = fs.images.odoo.dockerImage;
          nginx-image = fs.images.nginx.dockerImage;
          postgresql-image = firestream.packages.${system}.postgresql;
        };

        apps = {
          deploy-odoo = { type = "app"; program = "${odooChart}/bin/deploy"; };
          deploy-nginx = { type = "app"; program = "${nginxChart}/bin/deploy"; };
          deploy-cloudflared = { type = "app"; program = "${cloudflaredChart}/bin/deploy"; };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [ kubernetes-helm kubectl google-cloud-sdk pulumi-bin ];
          shellHook = ''
            echo "Firestream Cloudflare edge (GKE) example — edit ./config.nix, then see README.md" >&2
          '';
        };
      });
}
