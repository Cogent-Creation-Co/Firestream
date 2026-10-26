# Supabase pinning check flake-module
# Copyright Firestream. MIT License.
#
# Contributes:
#   - checks.supabase-no-prebuilt-binaries
#       (no machine code ships in a Supabase app output unless a Nix
#       derivation built it from source; see docs/nix-pinning-policy.md)
#
# The scan reads the app outputs only: the node_modules trees, the Elixir
# release trees, the Firestream Postgres extensions, and the Go, Haskell and
# Envoy builds. Base images and the rest of nixpkgs are the trusted base and
# stay out of it.
{ ... }: {
  perSystem = { pkgs, lib, config, firestreamLib, ... }:
    let
      module = image: (config.firestreamImages.${image}.eval (_: { })).module;

      runtimeDep = image: pname:
        let found = builtins.filter (d: (d.pname or null) == pname) (module image).runtimeDeps;
        in if found == [ ] then throw "supabase-no-prebuilt-binaries: ${image} has no runtime dependency ${pname}"
        else builtins.head found;

      storage = (module "supabase-storage").nodePackage;
      postgresMeta = (module "supabase-postgres-meta").nodePackage;
      studio = (module "supabase-studio").nodePackage;
      realtime = (module "supabase-realtime").release;
      supavisor = (module "supabase-supavisor").release;

      postgresEnv = lib.findFirst (d: d ? installedExtensions)
        (throw "supabase-no-prebuilt-binaries: supabase-postgres has no postgresql.withPackages runtime dependency")
        (module "supabase-postgres").runtimeDeps;
      firestreamExtensions =
        builtins.attrNames (firestreamLib.packages.postgresqlExtensionsFor pkgs.postgresql);
      extensions =
        builtins.filter (e: builtins.elem (e.pname or null) firestreamExtensions) postgresEnv.installedExtensions;

      # NOTE: each OTP application is named on its own. A wildcard over
      # `lib/*/priv` also matches a Hex dependency's priv directory, where
      # rustler_precompiled and elixir_make put downloaded NIFs.
      otp = release: {
        pattern = "(erts-[^/]+/bin|lib/(asn1|crypto|os_mon|runtime_tools)-[0-9.]+/priv/(bin|lib))/[^/]+";
        builtBy = release.beamPackages.erlang;
        reason = "ERTS and OTP NIFs that mix release copies from nixpkgs Erlang";
      };

      targets = [
        {
          image = "supabase-storage";
          drv = storage;
          allow = [
            {
              pattern = "lib/[^/]+/node_modules/\\.pnpm/fs-xattr@[^/]+/node_modules/fs-xattr/build/Release/xattr\\.node";
              builtBy = storage;
              reason = "node-gyp builds fs-xattr from its registry source";
            }
            {
              pattern = "lib/[^/]+/node_modules/\\.pnpm/@datadog\\+pprof@[^/]+/node_modules/@datadog/pprof/build/Release/dd_pprof\\.node";
              builtBy = storage.datadogPprof;
              reason = "node-gyp builds DataDog/pprof-nodejs from a pinned GitHub source";
            }
            {
              pattern = "lib/[^/]+/node_modules/\\.pnpm/source-map@[^/]+/node_modules/source-map/lib/mappings\\.wasm";
              builtBy = storage.sourceMapWasm;
              reason = "cargo builds wasm-mappings from the pinned mozilla/source-map source";
            }
          ];
        }
        {
          image = "supabase-postgres-meta";
          drv = postgresMeta;
          allow = [
            {
              pattern = "lib/[^/]+/node_modules/\\.pnpm/@oxfmt\\+binding-[^/]+/node_modules/@oxfmt/binding-[^/]+/oxfmt\\.[^/]+\\.node";
              builtBy = postgresMeta.oxfmtBinding;
              reason = "cargo builds the oxfmt napi binding from the pinned oxc source";
            }
            {
              pattern = "lib/[^/]+/node_modules/\\.pnpm/@sentry-internal\\+node-cpu-profiler@[^/]+/node_modules/@sentry-internal/node-cpu-profiler/(build/Release|lib)/sentry_cpu_profiler[^/]*\\.node";
              builtBy = postgresMeta;
              reason = "node-gyp builds the Sentry CPU profiler from its registry source";
            }
            {
              pattern = "lib/[^/]+/node_modules/\\.pnpm/libpg-query@[^/]+/node_modules/libpg-query/wasm/libpg-query\\.wasm";
              builtBy = postgresMeta.libpgQueryWasm;
              reason = "Emscripten builds libpg_query and libpg-query-node from pinned GitHub sources";
            }
          ];
        }
        {
          image = "supabase-studio";
          drv = studio;
          allow = [
            {
              pattern = "lib/[^/]+/node_modules/\\.pnpm/libpg-query@[^/]+/node_modules/libpg-query/wasm/libpg-query\\.wasm";
              builtBy = studio.libpgQueryWasm;
              reason = "Emscripten builds libpg_query and libpg-query-node from pinned GitHub sources";
            }
          ];
        }
        {
          image = "supabase-realtime";
          drv = realtime;
          allow = [ (otp realtime) ];
        }
        {
          image = "supabase-supavisor";
          drv = supavisor;
          allow = [
            (otp supavisor)
            {
              pattern = "lib/supavisor-[^/]+/priv/native/libpgparser\\.so";
              builtBy = supavisor;
              reason = "the rustler NIF that cargo builds from native/pgparser";
            }
          ];
        }
        {
          image = "supabase-auth";
          drv = runtimeDep "supabase-auth" "supabase-auth";
          allowAll = "buildGoModule build of supabase/auth";
        }
        {
          image = "supabase-imgproxy";
          drv = runtimeDep "supabase-imgproxy" "imgproxy";
          allowAll = "buildGoModule build of imgproxy";
        }
        {
          image = "supabase-postgrest";
          drv = runtimeDep "supabase-postgrest" "postgrest";
          allowAll = "nixpkgs Haskell build of PostgREST";
        }
        {
          image = "supabase-envoy";
          drv = runtimeDep "supabase-envoy" "envoy";
          allowAll = "Bazel build of Envoy";
        }
      ] ++ map
        (e: {
          image = "supabase-postgres";
          drv = e;
          allowAll = "PGXS build of the ${e.pname} extension";
        })
        extensions;

      entry = t: {
        name = t.image;
        root = "${t.drv}";
        drv = t.drv.name;
        allowAll = t.allowAll or null;
        allow = map
          (a:
            assert lib.assertMsg (lib.isDerivation a.builtBy)
              "supabase-no-prebuilt-binaries: ${t.image}: builtBy of `${a.pattern}` is not a derivation";
            { inherit (a) pattern reason; builtBy = a.builtBy.name; })
          (t.allow or [ ]);
      };

      scanConfig = pkgs.writeText "supabase-no-prebuilt-binaries.json"
        (builtins.toJSON (map entry targets));
    in
    {
      checks = lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
        supabase-no-prebuilt-binaries =
          pkgs.runCommand "supabase-no-prebuilt-binaries" { nativeBuildInputs = [ pkgs.python3 ]; } ''
            python ${../../bin/nix/firestream/supabase/scan-binaries.py} ${scanConfig}
            touch "$out"
          '';
      };
    };
}
