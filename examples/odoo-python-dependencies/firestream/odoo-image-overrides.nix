# ---------------------------------------------------------------------------
# The container-level "make it your own" surface — sibling of
# odoo-overrides.nix (which customizes the Helm CHART). Passed as the LAST
# module to `firestream.lib.<sys>.images.odoo.eval`, deep-merged on top of the
# Firestream container defaults to yield a CUSTOM Odoo image — without forking.
#
# HEADLINE: declare the Python packages your addons import IN-PROCESS as your
# own uv2nix workspace and MERGE them into Odoo's PRIMARY venv via
# `config.odoo.pythonWorkspace.extend`. Firestream composes ../python-workspace
# (pyproject.toml + committed uv.lock + overrides.nix) underneath its own
# odoo/18 workspace and builds ONE venv from both.
#
# This is deliberately NOT `extraWorkspaces` / a separate venv (the Airflow
# dagWorkspace shape): the Odoo process imports addon dependencies itself, so a
# sibling venv would be invisible to it.
#
# Three guards fire at build time, each with a legible error:
#   * requires-python must admit python312 (Odoo 18's interpreter);
#   * the two uv.lock files must agree on every shared package version, and the
#     extension's [project].name must not be "firestream-odoo";
#   * (as always) the committed uv.lock must be fresh (`uv lock --check`).
#
# Leave pythonWorkspace unset and the image is byte-for-byte the stock image.
# Option names mirror bin/nix/firestream/containers/eval-container.nix.
# ---------------------------------------------------------------------------
{ ... }:

{
  config.odoo.pythonWorkspace.extend = [
    {
      src = ../python-workspace; # self-contained uv2nix workspace
      # overrides = ../python-workspace/overrides.nix;  # auto-loaded from src
    }
  ];

  # Need to CHANGE a pin Firestream owns (not just add packages)? Own the whole
  # lock instead: copy src/containers/firestream/odoo/18/pyproject.toml, edit,
  # `uv lock --python 3.12`, then:
  #
  # config.odoo.pythonWorkspace.replace = {
  #   src = ../python-workspace-full;
  #   # inheritOverrides = true;  # keep Firestream's system-library build inputs
  # };
}
