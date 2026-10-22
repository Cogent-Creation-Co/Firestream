{ lib, ... }:

let
  inherit (lib) mkOption types;
in {
  options.odoo.managedSettings = mkOption {
    default = null;
    description = ''
      Platform settings that live outside Odoo. A release tool writes them to
      a Kubernetes Secret. The chart mounts that Secret read-only in the main
      Odoo container only and sets `FIRESTREAM_ODOO_MANAGED_DIR` to the mount
      path. An Odoo addon reads the files at boot.

      The chart cannot read the Secret at template time, so it adds no
      checksum annotation. To restart Odoo when the Secret changes, set
      `podAnnotations."checksum/managed-settings"` to a hash of the Secret data.
    '';
    type = types.nullOr (types.submodule {
      options = {
        existingSecret = mkOption {
          type = types.nullOr types.str;
          default = null;
          example = "odoo-managed-settings";
          description = "Name of an existing Secret in the release namespace. Each key becomes one file. Empty or null disables the mount and the env var.";
        };

        mountPath = mkOption {
          type = types.nullOr (types.strMatching "/.*");
          default = null;
          example = "/opt/firestream/odoo/managed";
          description = "Absolute directory for the Secret files and the value of `FIRESTREAM_ODOO_MANAGED_DIR` (chart default `/opt/firestream/odoo/managed`).";
        };
      };
    });
  };
}
