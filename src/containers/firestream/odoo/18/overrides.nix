# Odoo Python Package Overrides
# Copyright Firestream. MIT License.
#
# This file contains wheel and source build overrides for Odoo dependencies.
# These are necessary because some Python packages:
# - Need system libraries at runtime (wheelOverrides)
# - Don't have prebuilt Linux wheels and need build-time deps (sourceOverrides)

{ pkgs, lib }:

let
  # Build Python with common build tools
  buildPython = pkgs.python312.withPackages (ps: [ ps.setuptools ps.cython ps.wheel ]);
  setupPythonPath = ''
    export PATH="${buildPython}/bin:$PATH"
    export PYTHONPATH="${buildPython}/lib/python3.12/site-packages:''${PYTHONPATH:-}"
  '';
in
{
  # Wheel runtime overrides for C extensions needing system libs
  wheelOverrides = final: prev: {
    # lxml needs libxml2 and libxslt
    lxml = prev.lxml.overrideAttrs (old: {
      buildInputs = (old.buildInputs or [ ]) ++ [
        pkgs.libxml2
        pkgs.libxslt
      ];
    });

    # cryptography needs openssl and libffi
    cryptography = prev.cryptography.overrideAttrs (old: {
      buildInputs = (old.buildInputs or [ ]) ++ [
        pkgs.openssl
        pkgs.libffi
      ];
    });

    # psycopg2-binary may need PostgreSQL libs at runtime
    psycopg2-binary = prev.psycopg2-binary.overrideAttrs (old: {
      buildInputs = (old.buildInputs or [ ]) ++ [
        pkgs.postgresql.lib
      ];
    });

    # Pillow needs image libraries
    pillow = prev.pillow.overrideAttrs (old: {
      buildInputs = (old.buildInputs or [ ]) ++ [
        pkgs.libjpeg
        pkgs.libpng
        pkgs.zlib
        pkgs.freetype
      ];
    });

    # reportlab for PDF generation
    reportlab = prev.reportlab.overrideAttrs (old: {
      buildInputs = (old.buildInputs or [ ]) ++ [
        pkgs.freetype
      ];
    });
  };

  # Source build overrides for packages without Linux wheels
  sourceOverrides = final: prev: {
    # python-ldap needs OpenLDAP and SASL libraries
    python-ldap = prev.python-ldap.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
        buildPython
        pkgs.openldap.dev
        pkgs.cyrus_sasl.dev
        pkgs.pkg-config
      ];
      buildInputs = (old.buildInputs or [ ]) ++ [
        pkgs.openldap
        pkgs.cyrus_sasl
      ];
      postPatch = ''
        ${pkgs.gnused}/bin/sed -i 's/compile = 1/compile = 0/g' setup.cfg
        ${pkgs.gnused}/bin/sed -i 's/optimize = 1/optimize = 0/g' setup.cfg
      '';
      preBuild = ''
        export PYTHONPATH="${buildPython}/lib/python3.12/site-packages:''${PYTHONPATH:-}"
        export SETUPTOOLS_USE_DISTUTILS=local
      '';
    });

    # gevent needs special handling
    gevent = prev.gevent.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
        buildPython
        pkgs.pkg-config
      ];
      buildInputs = (old.buildInputs or [ ]) ++ [
        pkgs.libev
        pkgs.libuv
      ];
      preBuild = setupPythonPath;
    });

    # libsass for SCSS compilation
    libsass = prev.libsass.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
        buildPython
        pkgs.pkg-config
      ];
      buildInputs = (old.buildInputs or [ ]) ++ [
        pkgs.libsass
      ];
      preBuild = setupPythonPath;
    });

    # docopt - legacy package needs setuptools
    docopt = prev.docopt.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
        buildPython
      ];
      preBuild = setupPythonPath;
    });

    # ofxparse - legacy package needs setuptools
    ofxparse = prev.ofxparse.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
        buildPython
      ];
      preBuild = setupPythonPath;
    });

    # vobject - legacy package needs setuptools
    vobject = prev.vobject.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
        buildPython
      ];
      preBuild = setupPythonPath;
    });

    # xlwt - legacy package needs setuptools
    xlwt = prev.xlwt.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
        buildPython
      ];
      preBuild = setupPythonPath;
    });

    # polib - legacy package needs setuptools
    polib = prev.polib.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
        buildPython
      ];
      preBuild = setupPythonPath;
    });

    # ebaysdk - legacy package needs setuptools
    ebaysdk = prev.ebaysdk.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
        buildPython
      ];
      preBuild = setupPythonPath;
    });

    # rjsmin - legacy package needs setuptools
    rjsmin = prev.rjsmin.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
        buildPython
      ];
      preBuild = setupPythonPath;
    });

    # plaid-python - sdist-only package needs setuptools
    plaid-python = prev.plaid-python.overrideAttrs (old: {
      nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [
        buildPython
      ];
      preBuild = setupPythonPath;
    });
  };
}
