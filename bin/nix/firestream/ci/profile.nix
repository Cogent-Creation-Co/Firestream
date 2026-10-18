# Firestream's own CI profile
# Copyright Firestream. MIT License.
#
# The DATA half of the Nix -> JSON -> Rust contract: this module supplies values
# for the schema declared in ./eval-ci.nix, and ./lib/to-ci-manifest.nix turns it
# into `ci-manifest.json`. It is the CI-profile analogue of a chart's
# `src/charts/firestream/<name>/nix/options/` overlay — schema over there, values
# over here.
#
# Everything below is enumerated from the flake, not invented:
#
#   verify  -> the 13 granular checks in nix/flake-modules/checks.nix
#   build   -> the 11 container images in nix/flake-modules/containers/*
#              + the 10 chart bundles in nix/flake-modules/charts/*
#   attest  -> packages.manifest (the fleet SBOM, nix/flake-modules/aggregate.nix:38)
#
# NOTE ON `{system}`: every attribute below is a template. This module is
# evaluated once per system by nix/flake-modules/ci-profile.nix, and the emitted
# manifest carries `project.nix_system` for that system. A target that exists on
# only one system belongs in an `lib.optionals` here — NOT in a branch on the
# consumer side. That is the invariant the whole design turns on.

{ lib, config, checkNames ? [ ], ... }:

let
  # ── verify ────────────────────────────────────────────────────────────────
  # DERIVED from the flake's own `checks`, not hand-listed.
  #
  # This used to be a literal list of 13 names transcribed by hand, and it had
  # already drifted: the flake declares 23 checks, so the 9 `*-render-fidelity`
  # checks were never gated by CI at all. `nix flake check` could be red while
  # `make ci-check` was green — which is precisely how a batch of checks rotted
  # unnoticed. Enumerating `config.checks` makes a new check gated the day it is
  # added; removing one from the gate now requires an explicit entry below.
  #
  # checks.nix is Linux-gated (`lib.optionalAttrs isLinux`), so on Darwin
  # `config.checks` is empty and the verify phase comes out empty too — which is
  # what keeps the Darwin manifest at zero runnable phases. That property is
  # load-bearing: ci-linux treats "no runnable phases" as a hard error rather
  # than a silent green.
  excludedChecks = [
    # Aggregate of the same granular module tests we already gate individually
    # (= tests.all). Gating both doubles the work for one opaque verdict; the
    # granular set gives one pass/fail, one log and one span per module, which
    # is the entire reason the run dir exists.
    "firestream-tests"
  ];

  verifyChecks =
    builtins.filter (n: !(builtins.elem n excludedChecks)) checkNames;

  # ── build: container images ───────────────────────────────────────────────
  # The canonical (unversioned) alias of each container in
  # nix/flake-modules/containers/*.nix. The versioned siblings
  # (`airflow-3`, `postgresql-16`, `odoo-15`…) are aliases onto the same
  # derivations, so building them too would only duplicate work.
  #
  # 11, not the 10 the plan estimated: `os-shell` is a real container package
  # (nix/flake-modules/containers/os-shell.nix) and is included.
  containerPackages = [
    "airflow"
    "jupyterhub"
    "kafka"
    "nextjs"
    "odoo"
    "os-shell"
    "postgresql"
    "redis"
    "seaweedfs"
    "spark"
    "superset"
  ];

  # ── build: chart bundles ──────────────────────────────────────────────────
  # `packages.<name>-chart` is the Firestream-overlaid bundle (image injection +
  # path remaps + chart-manifest.json). The `<name>-base-chart` siblings are the
  # un-overlaid forks and are not part of the release surface, so they are not
  # built here. `packages.firestream-charts-bundle` (the aggregate symlink farm)
  # is added separately below — it is what FIRESTREAM_CHARTS_DIR points at.
  chartNames = [
    "airflow"
    "jupyterhub"
    "kafka"
    "nextjs"
    "odoo"
    "postgresql"
    "redis"
    "seaweedfs"
    "spark"
    "superset"
  ];

  pkgAttr = n: "packages.{system}.${n}";

  # ── e2e (Phase 9) ─────────────────────────────────────────────────────────
  # The two e2e harnesses this repo actually has, lifted out of "#[ignore]d
  # cargo test you must remember to run" into declared, advisory pipeline
  # phases with per-item spans, per-item log files and per-item verdicts.
  #
  # THE PARALLELISM FINDING, and why this is a CHAIN and not one phase full of
  # parallel tasks.
  #
  # `firestream_ci::pipeline::Pipeline::run` executes a phase's tasks with
  # `futures::future::join_all` — ALL of them, at once, with no concurrency
  # cap (pipeline/mod.rs). Meanwhile:
  #
  #   * `firestream-e2e-k8s` creates a REAL k3d cluster per chart
  #     (firestream-e2e-core/src/k8s/cluster.rs: `create_cluster`). Name
  #     collisions and port collisions are already engineered away — a 6-char
  #     random cluster suffix, `api_bind_address = 127.0.0.1`, and four
  #     `pick_ephemeral_port()` reservations per cluster — so N concurrent
  #     clusters would not *collide*. They would simply be N k3s servers, N
  #     registries and N full data-stack workloads on one machine. The crate's
  #     own `harness_lock()` mutex exists to stop exactly that within a
  #     process; the makefile's `--test-threads=1` reinforces it.
  #   * the docker harness (`src/lib/rust/firestream/tests/e2e.rs`) is worse:
  #     one process-wide `e2e_lock()` whose doc comment states the stacks
  #     "never race for host ports or for the docker daemon's image-load
  #     step".
  #
  # So "per-chart parallel tasks" is expressible here but would be actively
  # wrong. What the plan actually wants from parallel tasks — per-item
  # granularity in the dashboard — is obtained instead by giving each item its
  # OWN single-task phase and chaining the phases with `dependsOn`. Phases run
  # strictly in topological order, one at a time, so the chain reproduces the
  # harnesses' serialisation exactly, at profile level, with zero Rust change.
  #
  # The chain does NOT stop at the first failure: `Pipeline::run` only skips a
  # dependent when an upstream **required** phase failed. Every link here is
  # advisory, so a broken chart is reported and the sweep carries on — the same
  # behaviour as the cargo harness, where each chart is its own `#[test]`.
  #
  # `modes = [ "e2e" ]` is the safety interlock: these phases are invisible to
  # `--mode check` and `--mode release`, so no existing PR gate or release run
  # can trip over them. They materialise only under `firestream-ci ci-linux
  # --mode e2e` (`make ci-e2e`). See the long-form note in CLAUDE.md.

  # Canonical docker stacks, from `src/lib/rust/firestream/tests/e2e/stacks.rs`
  # (`CANONICAL`, 8 entries). Reordered cheapest-first so a chain that is going
  # to fail tends to fail early.
  e2eDockerStacks = [
    "postgresql"
    "redis"
    "kafka"
    "spark"
    "airflow"
    "jupyterhub"
    "superset"
    "odoo"
  ];

  # Canonical k8s charts, from `firestreamStacks.dev` / the 9 charts documented
  # in CLAUDE.md, plus `pg-backup` — the multi-chart backup/restore round-trip
  # which has its own explicit makefile target (`test-e2e-k8s-pg-backup` wins
  # over the `test-e2e-k8s-%` pattern rule) and therefore its own phase.
  e2eK8sCharts = [
    "postgresql"
    "redis"
    "seaweedfs"
    "kafka"
    "spark"
    "airflow"
    "jupyterhub"
    "superset"
    "odoo"
    "pg-backup"
  ];

  # One link of the chain: `{ name; target; }`. `target` is a makefile target
  # that ALREADY exists and whose env contract is documented in the makefile —
  # nothing here re-implements a harness, and `make test-e2e-k8s-redis` keeps
  # working byte-identically for incident reproduction.
  e2eLinks =
    (map (s: { name = "e2e-docker-${s}"; target = "test-e2e-${s}"; }) e2eDockerStacks)
    ++ (map (c: { name = "e2e-k8s-${c}"; target = "test-e2e-k8s-${c}"; }) e2eK8sCharts);

  # The phase the head of the chain hangs off. `verify` has no `modes`, so it
  # runs in mode `e2e` too: there is no point burning hours of cluster time on
  # a tree whose unit tests are red. It is `required`, so a red verify SKIPS
  # the whole chain (and the run exits 1) rather than running it.
  e2eChainHead = "verify";

  mkE2ePhase = i: link: {
    name = link.name;
    value = {
      tier = "advisory";
      order = 100 + i;
      modes = [ "e2e" ];
      # TWO edges, and the second one is not redundant.
      #
      #   * the previous link — this is what serialises the sweep;
      #   * `verify` DIRECTLY, on every link — this is what makes a red
      #     required gate skip the WHOLE chain.
      #
      # Skip does not propagate transitively in `Pipeline::run`: a phase is
      # skipped only when one of its OWN `depends_on` is a *required* phase
      # that failed. A skipped phase's `PhaseOutcome` has no tasks, so
      # `ok()` (an `all()` over an empty list) is TRUE, and its tier here is
      # advisory anyway — so with a chain-only edge, `verify` failing would
      # skip just the head link and then run all 17 remaining hours of e2e
      # against a tree whose unit tests are red. Naming `verify` on every
      # link is the data-only fix.
      dependsOn =
        [ e2eChainHead ]
        ++ lib.optional (i > 0) (builtins.elemAt e2eLinks (i - 1)).name;
      # Exactly ONE shell task per phase. This is the invariant that makes the
      # chain a serialisation: adding a second task to any of these phases
      # would put two harness runs on the same machine at the same time.
      shellTasks = [
        { name = link.name; command = [ "make" link.target ]; }
      ];
    };
  };

  e2ePhases = builtins.listToAttrs (lib.imap0 mkE2ePhase e2eLinks);

  # k3d, docker-compose stacks and every firestream-* image are Linux-only, and
  # nix/flake-modules/ci-profile.nix deliberately emits a Darwin manifest with
  # ZERO runnable phases so `ci-linux` on macOS is an explicit "not supported
  # here". Gate the e2e chain on the emitted system so `--mode e2e` does not
  # quietly re-introduce runnable phases into the Darwin manifest.
  #
  # Reading `config.ci.project.nixSystem` while defining `config.ci.phases` is
  # not recursive — different options — and `nixSystem` is supplied by
  # eval-ci.nix's `systemModule`, which has no dependency on `phases`.
  isLinux = lib.hasSuffix "-linux" config.ci.project.nixSystem;

  e2ePhasesFor = lib.optionalAttrs isLinux e2ePhases;

in {
  config.ci = {
    project = {
      name = "firestream";
      # nixSystem / arch are injected per-system by eval-ci.nix's systemModule.
      k8sNamespacePrefix = "firestream-";
    };

    # ── Pipeline shape ──────────────────────────────────────────────────────
    #   tidy (advisory) -> verify (required) -> build (required, release only)
    #                                        -> attest (advisory, release only)
    #
    #   mode=e2e only, appended by `e2ePhases` below:
    #   verify -> e2e-docker-postgresql -> ... -> e2e-k8s-pg-backup
    #             (18 advisory single-task phases, strictly chained)
    phases = e2ePhasesFor // {
      tidy = {
        tier = "advisory";
        order = 10;
        # No flake attrs: the tidy phase is store GC + repo-local sweep, both
        # driven by the runner rather than by a derivation. Both are named
        # from the runner's closed builtin vocabulary so nothing about this
        # phase is keyed off its NAME on the Rust side.
        #
        # `nix-gc` is load-bearing-by-omission on a native-first host: the
        # runner refuses to run the roots-based reaper against a host
        # /nix/store (that store IS the developer's working store, and also
        # IS the native build cache). Declaring it here is safe precisely
        # because the guard lives in the runner, not in this list.
        builtinTasks = [ "nix-gc" "target-sweep" ];
      };

      verify = {
        tier = "required";
        order = 20;
        dependsOn = [ "tidy" ];
        attrs = map (c: "checks.{system}.${c}") verifyChecks;
        # One nix-eval-jobs pass over `checks.{system}`, selecting exactly the
        # 13 leaves above, feeding one bounded build queue. The alternative is
        # 13 concurrent flake evaluations at 4 GB eval memory each.
        aggregate = true;
      };

      build = {
        tier = "required";
        order = 30;
        dependsOn = [ "verify" ];
        # Release-only. A check-mode PR gate runs tidy + verify and stops;
        # 21 container/chart builds do not belong on the critical path of a
        # pull-request check.
        modes = [ "release" ];
        attrs =
          (map pkgAttr containerPackages)
          ++ (map (n: pkgAttr "${n}-chart") chartNames)
          ++ [ (pkgAttr "firestream-charts-bundle") ];
        # 22 attrs, one common prefix (`packages.{system}`), no advisory
        # exceptions -> one nix-fast-build over the whole set. This is the
        # single largest wall-clock win available: one flake evaluation and
        # one bounded job queue instead of 22 of each.
        aggregate = true;
        # Materialise each built image/chart into <rundir>/artifacts/ with
        # sha256 + size, via the exportTargets rules below.
        exportArtifacts = true;
      };

      attest = {
        tier = "advisory";
        order = 40;
        dependsOn = [ "build" ];
        modes = [ "release" ];
        # The fleet SBOM (nix/flake-modules/aggregate.nix:38). Advisory: a
        # missing SBOM is worth reporting loudly but is not a reason to fail a
        # release whose images all built.
        attrs = [ (pkgAttr "manifest") ];
        # The whole point of the phase: the fleet SBOM lands in
        # <rundir>/artifacts/sbom/ with a sha256 and a manifest entry. Before
        # Phase 6 this phase merely ASSERTED that a file was already there.
        exportArtifacts = true;
      };
    };

    # ── Tier classification ─────────────────────────────────────────────────
    # Firestream deliberately declares NO tier rules. Unlike the reference
    # project — whose checks were named `required-*` / `advisory-*` so a prefix
    # classifier was the natural fit — every Firestream check is `firestream-*`
    # with no tier information in the name at all. Encoding tier here would mean
    # renaming 13 flake checks to satisfy the CI tool, which is backwards.
    #
    # So: everything defaults to `required`, and the exceptions are listed
    # explicitly in a phase's `advisoryAttrs`. Both mechanisms exist in the
    # schema precisely so a project can pick the one that matches its
    # conventions rather than adopting someone else's.
    tierRules = [ ];
    tierDefault = "required";

    # ── Export targets ──────────────────────────────────────────────────────
    # Ordered; first match wins.
    exportTargets = [
      # The chart bundles must be matched BEFORE the bare container names,
      # because `airflow-chart` also starts with `airflow`. Ordering, not a
      # more clever matcher, is what disambiguates — which is why list order
      # is part of the contract.
      #
      # `charts-bundle`, NOT `charts`. The aggregate symlink farm contains a
      # per-chart entry named `airflow`, `kafka`, … — exactly the names the
      # NEXT rule writes to under `charts/{stem}`. With both rooted at
      # `charts/`, two concurrently-finishing build tasks would `copy_tree`
      # into the same destination subtree, interleaving a resolved copy of the
      # farm with the individually-exported bundles. Harmless while nothing
      # actually exported; Phase 6 is the first phase that does. Separate
      # roots, no merge, no ordering dependency.
      {
        match = { equals = "firestream-charts-bundle"; };
        dest = "charts-bundle";
        kind = "nix";
      }
      {
        match = { suffix = "-chart"; };
        stripSuffix = "-chart";
        dest = "charts/{stem}";
        kind = "nix";
      }
      {
        match = { suffix = "-base-chart"; };
        stripSuffix = "-base-chart";
        dest = "charts-base/{stem}";
        kind = "nix";
      }
      # The fleet SBOM.
      {
        match = { equals = "manifest"; };
        dest = "sbom";
        kind = "sbom";
      }
      # Everything the build phase produces that is not a chart or the SBOM is
      # a container image tarball, one directory per image, arch-qualified so a
      # multi-arch rundir does not collide.
      {
        match = { };
        dest = "images/{leaf}-{arch}";
        kind = "image";
      }
    ];
    # Unreachable given the catch-all above, but the schema wants a total
    # function and an explicit fallback documents the intent.
    exportDefault = {
      dest = "{leaf}";
      kind = "binary";
    };

    # ── Host -> builder env allowlist ───────────────────────────────────────
    # Only names that actually mean something in this repo. `FIRESTREAM_CI_PROFILE`
    # is appended by the consumer whether or not it is listed, so the inner run
    # resolves the same manifest as its parent.
    passthroughVars = [
      "BRANCH"
      "BRANCH_NAME"
      "GIT_SHA"
      "GIT_COMMIT_HASH"
      "IMAGE_TAG"
      "AR_REGISTRY"
      "CI_MODE"
      "CI_RUNNER"
      "CONTAINER_ARCH"
      "BUILDER_IMAGE_NAME"
      "FIRESTREAM_BUILD_STRATEGY"
      "FIRESTREAM_CHARTS_DIR"
      "NIX_SYSTEM"
      "NIX_BUILD_CORES"
      "NIX_BUILD_MAX_JOBS"
      "NIX_MAX_SUBSTITUTION_JOBS"
      "NIX_HTTP_CONNECTIONS"
      "OTEL_EXPORTER_OTLP_ENDPOINT"
      "OTEL_SPAN_DIR"
      "TRACEPARENT"
      "HONEYCOMB_API_KEY"
      "HONEYCOMB_DATASET"
      "SWEEP_DAYS"

      # ── e2e harness contract (Phase 9) ──────────────────────────────────
      # The `e2e-*` phases shell out to the SAME makefile targets a developer
      # runs by hand, and those targets are configured purely by env. Listing
      # the vars here is what makes `FIRESTREAM_E2E_K8S_STRICT=1 make ci-e2e`
      # behave identically when the run is lifted into the docker builder.
      #
      # `*_STRICT=1` is the one worth setting deliberately in an unattended
      # run: without it a machine missing k3d/helm/docker makes every harness
      # test SKIP — and skip is GREEN. A green sweep that tested nothing is
      # the worst possible CI signal.
      "FIRESTREAM_E2E_STACKS"
      "FIRESTREAM_E2E_KEEP"
      "FIRESTREAM_E2E_STRICT"
      "FIRESTREAM_E2E_TIMEOUT_SECS"
      "FIRESTREAM_E2E_PREBUILD"
      "FIRESTREAM_E2E_HTTP"
      "FIRESTREAM_E2E_K8S_STACKS"
      "FIRESTREAM_E2E_K8S_KEEP"
      "FIRESTREAM_E2E_K8S_STRICT"
      "FIRESTREAM_E2E_K8S_TIMEOUT_SECS"
      "FIRESTREAM_E2E_K8S_PRELOAD"
      "FIRESTREAM_E2E_K8S_CHARTS_DIR"
      "FIRESTREAM_E2E_K8S_HELM_TIMEOUT"
    ];

    # ── Devshell sentinels ──────────────────────────────────────────────────
    # `IN_NIX_SHELL` is Nix's own (set to `pure`/`impure`, so presence-only).
    # `FIRESTREAM_DEVSHELL` is ours and is exported as exactly `1` by
    # nix/flake-modules/devshell.nix.
    devshellSentinels = {
      IN_NIX_SHELL = { order = 10; };
      FIRESTREAM_DEVSHELL = { value = "1"; order = 20; };
    };

    builder = {
      imageName = "firestream-builder";
      baseImage = "nixos/nix:latest";
      minImageSizeBytes = 104857600; # 100 MiB
      containerNamePrefix = "firestream-ci-";
    };

    # Part C of the plan. Consumed by Phase 5's `platform` unification; declared
    # now so the schema does not need a version bump then. `{arch}` is expanded
    # by the consumer, matching bin/build/strategy.sh's volume naming.
    buildStrategy = {
      default = "auto";
      nativeCache = "host-store";
      # NOTE ON `{arch}`: this template expands to the DOCKER arch alias
      # (amd64 / arm64), NOT the normalised Nix arch (x86_64 / aarch64),
      # because it has to name the same volume `get_nix_volume` in
      # bin/build/strategy.sh already created. The consumer that expands it is
      # `firestream_ci::platform::docker_cache_volume`, which is driven over
      # bin/build/strategy-cases.json's `arch_cases[].nix_volume` — the same
      # golden vectors the shell half is driven over.
      dockerCacheVolume = "firestream-nix-store-{arch}";
    };

    # ── Container -> Nix package table ──────────────────────────────────────
    # Read verbatim from the golden-vector file so the Nix producer, the Rust
    # consumer and bin/build/_common.sh's CONTAINER_REGISTRY cannot drift.
    # bin/build/test-registry-parity.sh gates the bash side against this same
    # file in both directions.
    #
    # THE LANDMINE: `.#redis` is redis-8 (nix/flake-modules/containers/redis.nix)
    # but a bare `redis` here is redis-7. That is deliberate and load-bearing —
    # `make redis-7-start` rebuild-loops if it changes.
    containerRegistry =
      (builtins.fromJSON (builtins.readFile ../../../build/registry-cases.json)).entries;
  };
}
