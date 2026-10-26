# Firestream Packages Index
# Copyright Firestream. MIT License.
#
# This module provides custom packages built from source within the Firestream
# repository. These packages are used as dependencies in the container module system.
#
# mkRustPackage is provided by the Rust module (Fenix + Crane) for incremental
# builds with dependency caching.
#
# Each package is wrapped with fleet manifest support attributes:
# - metadata: derivation containing SBOM and metadata files
# - isFirestreamArtifact: true for Rust CLI tools (distinguishes from containers)

{ pkgs, lib, mkRustPackage, fenix, system }:

let
  goToolchain = import ./go-toolchain.nix { inherit pkgs lib; };

  # Raw Rust packages without fleet manifest wrappers
  # These are the actual derivations built from source
  rawPackages = {
    firestream = import ./firestream.nix { inherit pkgs lib mkRustPackage; };
    wait-for-port = import ./wait-for-port.nix { inherit pkgs lib mkRustPackage; };
    firestream-vib = import ./firestream-vib.nix { inherit pkgs lib mkRustPackage; };
    firestream-healthd = import ./firestream-healthd.nix { inherit pkgs lib mkRustPackage; };
  };

  # Import metadata library for generating Rust package metadata
  # Uses the raw firestream-vib package (unwrapped) to generate metadata
  metadataLib = import ../lib/metadata.nix {
    inherit pkgs lib;
    firestreamVibPkg = rawPackages.firestream-vib;
  };

  # Helper function to wrap a Rust package with fleet manifest attributes
  # For CLI tools, we generate metadata based on the built derivation
  wrapRustPackage = { name, package, version ? "0.1.0", description ? "" }:
    let
      # Generate metadata for Rust binary packages
      # This produces SBOM files for the Rust package and its Nix closure
      metadata = metadataLib.mkContainerMetadata {
        inherit name version;
        mainDrv = package;
        # CLI tools don't have ports, users, or working directories
        exposedPorts = [];
        user = "0:0";  # CLI tools typically run as the invoking user
        workdir = "/";
      };
    in
    package // {
      # Expose metadata derivation for fleet SBOM aggregation
      inherit metadata;

      # Expose package list for fleet-level source introspection
      packageList = [ package ];

      # Tag for dynamic filtering (distinguishes from containers)
      isFirestreamArtifact = true;

      # Package metadata
      meta = (package.meta or {}) // {
        inherit name version description;
      };
    };

in {
  # firestream: CLI/TUI for managing data infrastructure services
  # Usage: firestream [COMMAND] or launch the TUI
  firestream = wrapRustPackage {
    name = "firestream-tui";
    package = rawPackages.firestream;
    version = "0.1.0";
    description = "CLI/TUI tool for managing data infrastructure services";
  };

  # wait-for-port: Rust-based port availability checker
  # Usage: wait-for-port PORT [--host HOST] [--state inuse|free] [--timeout SECS]
  wait-for-port = wrapRustPackage {
    name = "wait-for-port";
    package = rawPackages.wait-for-port;
    version = "0.1.0";
    description = "Wait for a TCP port to reach a desired state (inuse or free)";
  };

  # firestream-vib: Container verification harness and metadata generator
  # Usage: firestream-vib generate-metadata --closure-graph FILE --config FILE --output DIR
  firestream-vib = wrapRustPackage {
    name = "firestream-vib";
    package = rawPackages.firestream-vib;
    version = "0.1.0";
    description = "Container verification harness and metadata generator";
  };

  # firestream-healthd: tiny in-image health / SBOM HTTP server
  # Usage: firestream-healthd [--port 9180] [--readiness-cmd '...'|--readiness-port N|--readiness-http URL]
  # Phase 2: shipped in every container's Nix store but NOT yet launched by the entrypoint.
  firestream-healthd = wrapRustPackage {
    name = "firestream-healthd";
    package = rawPackages.firestream-healthd;
    version = "0.1.0";
    description = "In-container health/SBOM HTTP server (/healthz, /readyz, /metadata, /sbom, /closure)";
  };

  # seaweedfs-weed: the SeaweedFS `weed` binary (Apache-2.0, single Go binary,
  # S3-compatible object store). Built via buildGoModule, NOT a Rust package, so
  # it is exposed directly (no wrapRustPackage / SBOM metadata wrapper). Consumed
  # by the seaweedfs container's runtimeBinDeps (threaded via the container
  # flake-module's extraFactoryArgs).
  seaweedfs-weed = import ./seaweedfs-weed.nix { inherit pkgs lib; };

  # imgproxy at the Supabase-pinned version (buildGoModule, exposed directly
  # like seaweedfs-weed). Consumed by the imgproxy container.
  imgproxy = import ./imgproxy.nix { inherit pkgs lib goToolchain; };

  # supabase-auth: Supabase Auth (GoTrue), built from a pinned upstream rev.
  supabase-auth = import ./supabase-auth.nix { inherit pkgs lib goToolchain; };

  # envoy: Envoy for the Supabase gateway, built offline by Bazel from
  # per-dependency pins (envoy/deps.json).
  envoy = import ./envoy { inherit pkgs lib fenix system; };

  # goToolchain: { go; buildGoModule; } at a Go newer than the pin's, for
  # supabase-auth and imgproxy.
  inherit goToolchain;

  # postgresqlExtensionsFor: Postgres extensions that nixpkgs does not ship
  # (supautils, supabase_vault, plan_filter), built against the given
  # `postgresql_<N>`. A function, not a package, because an extension must be
  # built against the exact server it loads into. Consumed by the postgresql
  # container's `extensions` option, which merges the set into
  # `postgresql.pkgs`.
  # Usage: (postgresqlExtensionsFor pkgs.postgresql_17).supabase_vault
  postgresqlExtensionsFor = import ./postgresql-extensions { inherit pkgs lib; };
}
