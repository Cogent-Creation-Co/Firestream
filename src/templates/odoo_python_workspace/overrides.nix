# Overrides for the odoo_python_workspace extension fixture
# Copyright Firestream. MIT License.
#
# Same call contract as every Firestream <workspace>/overrides.nix:
#   { pkgs, lib }: { wheelOverrides = final: prev: {...}; sourceOverrides = final: prev: {...}; }
# Firestream auto-loads this file from the workspace dir and composes it AFTER
# the base container's overrides, so `prev` already carries Firestream's fixups.

{ pkgs, lib }:

{
  wheelOverrides = final: prev: {
    # freetype-py ships a manylinux wheel that bundles libfreetype; the bundled
    # library still dlopens the system codecs it was linked against.
    freetype-py = prev.freetype-py.overrideAttrs (old: {
      buildInputs = (old.buildInputs or [ ]) ++ [
        pkgs.freetype
        pkgs.zlib
        pkgs.libpng
        pkgs.bzip2
        pkgs.brotli
      ];
    });
  };

  sourceOverrides = final: prev: {
    # pycairo publishes no Linux wheels. It builds from sdist with meson-python
    # and needs the cairo headers via pkg-config. `resolveBuildSystem` pulls the
    # backend (and meson/ninja) from pyproject-build-systems.
    pycairo = prev.pycairo.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or [ ])
        ++ final.resolveBuildSystem { meson-python = [ ]; }
        ++ [ pkgs.pkg-config pkgs.meson pkgs.ninja ];
      buildInputs = (old.buildInputs or [ ]) ++ [
        pkgs.cairo
      ];
    });
  };
}
