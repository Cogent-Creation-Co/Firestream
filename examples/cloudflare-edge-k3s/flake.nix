{
  description = "Firestream Cloudflare edge — Odoo behind nginx behind a Cloudflare Tunnel, on local k3s / k3d";

  # ---------------------------------------------------------------------------
  # THE FIRST MULTI-CHART EXAMPLE. Every other example deploys one Helm release;
  # this one composes three into a single namespace:
  #
  #   Cloudflare edge --> cloudflared Deployment --> nginx Service :80 --> odoo
  #                       (outbound only)            (per-namespace proxy)   :8069 HTTP
  #                                                                         :8072 websocket
  #
  # Each chart is still reached through the same `charts.<app>.eval` seam, and
  # each override is still a strict subset of that chart's value surface. What
  # is new is that they must AGREE with each other: nginx's `upstreams` name
  # Odoo's Service and ports, and Odoo must be running in the mode that actually
  # binds those ports. config.nix is the single place those shared facts live.
  # ---------------------------------------------------------------------------
  inputs = {
    firestream.url = "github:Cogent-Creation-Co/Firestream-one-flake-to-rule-them-all";

    nixpkgs.follows = "firestream/nixpkgs";
    flake-utils.follows = "firestream/flake-utils";
  };

  outputs = { self, firestream, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        fs = firestream.lib.${system};

        odooChart = (fs.charts.odoo.eval
          (import ./firestream/odoo-overrides.nix)).chartBundle;

        nginxChart = (fs.charts.nginx.eval
          (import ./firestream/nginx-overrides.nix)).chartBundle;

        # cloudflared is a CHART-ONLY app: there is no
        # src/containers/firestream/cloudflared/ and no
        # `firestream.lib.<sys>.images.cloudflared`, because it runs
        # Cloudflare's own connector image rather than a Nix-built one. So this
        # is the one chart here with no matching `*-image` package below, and
        # nothing for deploy-local.sh to side-load -- the kubelet pulls it from
        # docker.io like any upstream image.
        cloudflaredChart = (fs.charts.cloudflared.eval
          (import ./firestream/cloudflared-overrides.nix)).chartBundle;
      in
      {
        packages = {
          default = nginxChart;

          odoo-chart = odooChart;
          nginx-chart = nginxChart;
          cloudflared-chart = cloudflaredChart;

          # Images to side-load. nginx's image IS Nix-built (unlike
          # cloudflared's), so it needs the same treatment as Odoo's.
          odoo-image = fs.images.odoo.dockerImage;
          nginx-image = fs.images.nginx.dockerImage;
          postgresql-image = firestream.packages.${system}.postgresql;
        };

        apps = {
          # Per-chart deploy wrappers. ORDER MATTERS -- see scripts/deploy-local.sh
          # and the README; `make deploy` runs all three in the right sequence.
          deploy-odoo = { type = "app"; program = "${odooChart}/bin/deploy"; };
          deploy-nginx = { type = "app"; program = "${nginxChart}/bin/deploy"; };
          deploy-cloudflared = { type = "app"; program = "${cloudflaredChart}/bin/deploy"; };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [ kubernetes-helm kubectl k3d ];
          shellHook = ''
            echo "Firestream Cloudflare edge (local k3s) example — edit ./config.nix, then 'make deploy'" >&2
          '';
        };
      });
}
