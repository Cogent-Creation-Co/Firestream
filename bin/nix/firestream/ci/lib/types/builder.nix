# CI profile shared types: builder image, build strategy, env sentinels
# Copyright Firestream. MIT License.
#
# The scalar/leaf end of the schema — the values that were `const` in the
# deleted defaults module:
#
#   BUILDER_IMAGE_NAME            -> builder.imageName
#   NIX_BASE_IMAGE                -> builder.baseImage
#   MIN_BUILDER_IMAGE_SIZE_BYTES  -> builder.minImageSizeBytes
#   (bash `docker rm` name filter) -> builder.containerNamePrefix
#
# plus `buildStrategy` (Part C of the plan: native-vs-docker build and cache
# policy — carried in schema v1 so Phase 5's `platform` unification does not
# need a version bump) and `envSentinelType` (the devshell guard's env probes).

{ lib, ... }:

let
  inherit (lib) mkOption types;

in {
  builderType = types.submodule {
    options = {
      imageName = mkOption {
        type = types.str;
        default = "";
        description = ''
          Bare builder-image name, e.g. `firestream-builder`. Used as the
          local tag stem (`<imageName>:<branch>-<arch>`) and as the reaper's
          `reference=` filter scope. Empty means "the caller must pass
          `--image-name` / `BUILDER_IMAGE_NAME`", and the CLI errors rather
          than guessing.
        '';
        example = "firestream-builder";
      };

      baseImage = mkOption {
        type = types.str;
        default = "";
        description = "Base image for cold-start builder creation.";
        example = "nixos/nix:latest";
      };

      minImageSizeBytes = mkOption {
        type = types.int;
        default = 104857600; # 100 MiB
        description = ''
          Minimum legitimate flattened builder-image size. Anything smaller is
          rejected by the flatten/retag step as a likely corrupt or empty
          image, leaving the previous canonical tag untouched.
        '';
      };

      containerNamePrefix = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Prefix for transient CI container names, and the `name=` filter the
          orphan reaper uses. `null` => the consumer derives `"<project>-ci-"`.
          Never emit an empty string: the reaper would then match every stopped
          container on the host (the Rust side refuses that case defensively).
        '';
        example = "firestream-ci-";
      };
    };
  };

  buildStrategyType = types.submodule {
    options = {
      default = mkOption {
        type = types.enum [ "auto" "native" "docker" ];
        default = "auto";
        description = ''
          Default build backend. `auto` probes the host (Linux + nix + a usable
          store => native, else docker). Mirrors `FIRESTREAM_BUILD_STRATEGY`,
          which is read before any probing so forcing `docker` is a total
          rollback with no code revert.
        '';
      };

      nativeCache = mkOption {
        type = types.str;
        default = "";
        description = "Cache backend used by the native strategy.";
        example = "host-store";
      };

      dockerCacheVolume = mkOption {
        type = types.str;
        default = "";
        description = ''
          Docker volume name holding the persistent per-arch Nix store.
          `{arch}` is expanded by the consumer.
        '';
        example = "firestream-nix-store-{arch}";
      };
    };
  };

  # An env-var probe. `value = null` means "present with any value"; a non-null
  # value means "present and exactly equal".
  #
  # Both forms are required by the behaviour this replaces: the devshell guard
  # tested `IN_NIX_SHELL` for PRESENCE (Nix sets it to `pure` or `impure`, never
  # `1`) but the project's own marker for the exact string `1`.
  envSentinelType = types.submodule ({ name, ... }: {
    options = {
      name = mkOption {
        type = types.str;
        default = name;
        description = "Environment variable name. Defaults to the attribute key.";
      };
      value = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Required exact value; null means presence-only.";
      };
      order = mkOption {
        type = types.int;
        default = 100;
        description = "Sort key for deterministic emission only.";
      };
    };
  });
}
