{
  description = "Firestream Odoo with extra in-process Python dependencies — local k3s / k3d";

  # ---------------------------------------------------------------------------
  # Same one-flake pattern as ../odoo-k3s. The difference is in
  # firestream/odoo-image-overrides.nix: the custom image merges a consumer-owned
  # uv2nix workspace (./python-workspace) into Odoo's PRIMARY venv via
  # config.odoo.pythonWorkspace.extend, and the chart override sets the
  # production-shaped odoo.conf limits. Images are side-loaded into the
  # cluster's containerd (see ./scripts/deploy-local.sh).
  # ---------------------------------------------------------------------------
  inputs = {
    # The Firestream monorepo — the only input a downstream repo pins. The bare
    # URL tracks the default branch (`main`); pin `?ref=<tag-or-commit>` for
    # reproducibility in real use.
    firestream.url = "github:Cogent-Creation-Co/Firestream-one-flake-to-rule-them-all";

    # Re-use Firestream's pinned nixpkgs / flake-utils so versions match.
    nixpkgs.follows = "firestream/nixpkgs";
    flake-utils.follows = "firestream/flake-utils";
  };

  outputs = { self, firestream, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };

        # The customization seam: re-evaluate the stock Odoo chart with our
        # local override deep-merged on top of the Firestream defaults (and the
        # image-injection overlay). Returns { chartBundle; render; config; ... }.
        odoo = firestream.lib.${system}.charts.odoo.eval
          (import ./firestream/odoo-overrides.nix);

        # The deployable bundle: $out/{chart,values.yaml,chart-manifest.json}
        # plus an executable $out/bin/deploy (helm upgrade --install wrapper).
        odooChart = odoo.chartBundle;
      in
      {
        packages = {
          default = odooChart;
          # The customized Odoo chart bundle (build + inspect with `nix build`).
          odoo-chart = odooChart;

          # CUSTOM Odoo image: re-evaluate the Firestream Odoo container with our
          # own preferences (firestream/odoo-image-overrides.nix — the extra Python
          # dependencies). The result is still named `firestream-odoo:18.0`, so
          # the chart's default-injected image ref (which we deliberately do NOT
          # repoint in odoo-overrides.nix) picks it up once it's side-loaded into
          # the cluster. (Linux-only; macOS yields a build stub but evaluation is
          # always safe.)
          odoo-image = (firestream.lib.${system}.images.odoo.eval
            (import ./firestream/odoo-image-overrides.nix)).dockerImage;
          postgresql-image = firestream.packages.${system}.postgresql;
        };

        apps = {
          # `nix run .#deploy -- --namespace odoo [extra helm args]`. NOTE: this
          # only runs `helm upgrade --install`; it does NOT side-load images. Use
          # `make deploy` (scripts/deploy-local.sh) for the full local loop.
          deploy = {
            type = "app";
            program = "${odooChart}/bin/deploy";
          };
        };

        devShells.default = pkgs.mkShell {
          packages = with pkgs; [
            kubernetes-helm
            kubectl
            k3d # optional: spin up a throwaway local cluster
            uv # regenerate ./python-workspace/uv.lock
          ];
          shellHook = ''
            echo "Firestream Odoo + Python deps example — edit ./python-workspace, then 'make deploy'" >&2
          '';
        };
      });
}
