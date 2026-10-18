# Chart Evaluation Mechanism
# Copyright Firestream. MIT License.
#
# Options-driven entrypoint for evaluating a single Helm chart and building a
# self-contained, offline-rendered deployment bundle. It mirrors the container
# evaluator (bin/nix/firestream/containers/eval-container.nix): declare a
# standard option schema under the chart's name, evaluate it together with the
# chart's own options modules, serialise the resulting `values` tree to a
# values.yaml, vendor the chart's subcharts from the in-repo fork, and assemble
# a chart bundle whose build is GATED by an in-sandbox `helm template`.
#
# STRUCTURAL INVERSION vs containers: eval-chart declares ONLY `_meta`/`values`.
# The ~3000 typed option paths live in the chart's own option modules (added in
# later phases), which `import ../types { inherit lib; }`. specialArgs therefore
# passes `{ inherit pkgs lib; }` so those modules can build their typed schema.
#
# It is NOT wired into the flake outputs in Phase 1 — framework.nix injects it
# as `evalChart`, and Phase 4 wires a charts/airflow.nix flake-module.
#
# Signature:
#   { pkgs, lib, firestreamLib }:
#     { name, chartSrc, modules ? [], subcharts ? [],
#       helm ? pkgs.kubernetes-helm, releaseName ? name, namespace ? name }:
#
# Returns:
#   { chartBundle; valuesYaml; chartManifest; render; config; options; }
#
# Phase 1 (chart manifest): the bundle now additionally emits a
# `chart-manifest.json` at `$out/chart-manifest.json` describing the deployment
# intent (release, deployment flags, lifecycle, container refs, provenance).
# The JSON schema (v1) is the contract every downstream consumer reads from.
# Options declared here under `_meta.{deployment,lifecycle,containerRefs,provenance}`
# carry default values that mirror the schema doc. `_meta.containerRefs` is
# intentionally left as an empty default - Agent B owns image injection.
{ pkgs, lib, firestreamLib }:

{
  name,
  chartSrc,
  modules ? [],
  subcharts ? [],
  helm ? pkgs.kubernetes-helm,
  releaseName ? name,
  namespace ? name,
}:

let
  # Standard option schema declared under the ${name} namespace. The chart's own
  # option modules (later phases) declare the concrete value paths under
  # `config.${name}.values.*`.
  mkChartOptions = { name }: { lib, ... }: {
    options.${name} = {
      _meta = lib.mkOption {
        description = "Chart deployment metadata (release name, namespace, deployment knobs, lifecycle, provenance).";
        default = {};
        type = lib.types.submodule {
          options = {
            chartName = lib.mkOption {
              type = lib.types.str;
              default = name;
              description = "Helm chart name.";
            };
            releaseName = lib.mkOption {
              type = lib.types.str;
              default = releaseName;
              description = "Helm release name used for template/upgrade.";
            };
            namespace = lib.mkOption {
              type = lib.types.str;
              default = namespace;
              description = "Kubernetes namespace the release is deployed into.";
            };
            createNamespace = lib.mkOption {
              type = lib.types.bool;
              default = true;
              description = ''
                Whether the deploying layer creates the namespace: `--create-namespace`
                on the emitted `bin/deploy`, and `release.createNamespace` in
                chart-manifest.json (which the Rust executor reads).

                Set false when something else OWNS namespace lifecycle -- a Pulumi
                stack that also attaches the ResourceQuota, LimitRange,
                NetworkPolicy and Workload Identity ServiceAccount. Letting Helm
                conjure a bare, unquota'd namespace would race that.

                Default true because the local flows depend on it: every `*-k3s`
                example runs `./result/bin/deploy --namespace "$NS"` with nothing
                having created `$NS` first.
              '';
            };

            # Helm CLI knobs. These surface as `deployment.*` in the manifest
            # (chart-manifest.json) and drive the Rust deploy layer (Agent D).
            deployment = lib.mkOption {
              description = "Helm install/upgrade behavioural flags.";
              default = {};
              type = lib.types.submodule {
                options = {
                  atomic = lib.mkOption {
                    type = lib.types.bool;
                    default = false;
                    description = "Pass --atomic: roll back the release if upgrade fails.";
                  };
                  wait = lib.mkOption {
                    type = lib.types.bool;
                    default = false;
                    description = "Pass --wait: block until all resources are in a ready state.";
                  };
                  waitForJobs = lib.mkOption {
                    type = lib.types.bool;
                    default = false;
                    description = "Pass --wait-for-jobs: also wait for all Jobs to complete.";
                  };
                  timeout = lib.mkOption {
                    type = lib.types.str;
                    default = "5m";
                    description = "Pass --timeout: max duration to wait for release to be ready.";
                  };
                  forceUpgrade = lib.mkOption {
                    type = lib.types.bool;
                    default = false;
                    description = "Pass --force on upgrades: forcibly replace resources.";
                  };
                  hooksDisabled = lib.mkOption {
                    type = lib.types.bool;
                    default = false;
                    description = "Pass --no-hooks: skip Helm hooks for this release.";
                  };
                  skipCrds = lib.mkOption {
                    type = lib.types.bool;
                    default = false;
                    description = "Pass --skip-crds: do not install CRDs.";
                  };
                };
              };
            };

            # Cross-release lifecycle metadata. Consumed by the Rust deploy
            # layer's lifecycle traits (helm_lifecycle/lifecycle.rs).
            lifecycle = lib.mkOption {
              description = "Cross-release lifecycle metadata.";
              default = {};
              type = lib.types.submodule {
                options = {
                  dependsOn = lib.mkOption {
                    type = lib.types.listOf lib.types.str;
                    default = [];
                    description = "Names of other releases this chart depends on (deploy ordering).";
                  };
                  lastBreakingVersion = lib.mkOption {
                    # `null` => no breaking-version handler registered.
                    # When non-null, the structure matches Rust's BreakingVersion
                    # (types.rs): { version; requires_uninstall; pre_upgrade_commands; }.
                    type = lib.types.nullOr (lib.types.submodule {
                      options = {
                        version = lib.mkOption {
                          type = lib.types.str;
                          description = "SemVer string below which the breaking-change handler runs.";
                        };
                        requiresUninstall = lib.mkOption {
                          type = lib.types.bool;
                          default = false;
                          description = "If true, uninstall the existing release before re-installing.";
                        };
                        preUpgradeCommands = lib.mkOption {
                          type = lib.types.listOf lib.types.str;
                          default = [];
                          description = "kubectl/helm commands to run before the upgrade.";
                        };
                      };
                    });
                    default = null;
                    description = "Last chart version that requires special pre-upgrade handling; null => no handler.";
                  };
                };
              };
            };

            # Image override map; Agent B wires the container registry into
            # this attrset. Keys are chart-relative image slot identifiers
            # (e.g. "airflow" for the main image, "redis" for the redis
            # subchart image); values are `{ registry; repository; tag;
            # componentPath; }`-shaped submodules with all string fields
            # nullable so each can be selectively overridden.
            #
            # `componentPath` (added by Phase 2) is the list of attribute
            # names locating the chart's image slot in the values tree
            # (e.g. [ "image" ] for the airflow top-level image, [ "redis"
            # "image" ] for a redis subchart image). When non-empty, the
            # injector (lib/inject-container-images.nix) writes the
            # { registry; repository; tag; } triple at that path before
            # values.yaml is emitted. When empty, the slot is recorded in
            # chart-manifest.json but no values overlay is produced - useful
            # for catalogue-only entries.
            containerRefs = lib.mkOption {
              default = {};
              description = "Image overrides populated by the container registry; merged into the values tree by inject-container-images.nix.";
              type = lib.types.attrsOf (lib.types.submodule {
                options = {
                  registry = lib.mkOption {
                    type = lib.types.nullOr lib.types.str;
                    default = null;
                    description = "Image registry (e.g. docker.io).";
                  };
                  repository = lib.mkOption {
                    type = lib.types.nullOr lib.types.str;
                    default = null;
                    description = "Image repository (e.g. bitnami/airflow).";
                  };
                  tag = lib.mkOption {
                    type = lib.types.nullOr lib.types.str;
                    default = null;
                    description = "Image tag.";
                  };
                  componentPath = lib.mkOption {
                    type = lib.types.listOf lib.types.str;
                    default = [];
                    description = "Attribute path in the values tree where the image triple is injected (e.g. [ \"image\" ], [ \"redis\" \"image\" ]). Empty list = manifest entry only, no values overlay.";
                  };
                };
              });
            };

            # Backup contract consumed by `firestream helm backup|restore`.
            # Null means the chart ships no backup CronJob. `cronJobSuffix` is
            # appended to the Bitnami fullname (`<fullname>-<suffix>`) to find
            # the CronJob; `quiesceDeployment` makes the restore scale the
            # `<fullname>` Deployment to zero first and back up afterwards.
            backup = lib.mkOption {
              default = null;
              description = "Backup/restore CronJob contract, or null when the chart has none.";
              type = lib.types.nullOr (lib.types.submodule {
                options = {
                  cronJobSuffix = lib.mkOption {
                    type = lib.types.str;
                    description = "Suffix after the Bitnami fullname naming the backup CronJob.";
                  };
                  quiesceDeployment = lib.mkOption {
                    type = lib.types.bool;
                    default = false;
                    description = "Scale the app Deployment to zero around a restore.";
                  };
                };
              });
            };

            # Build-time provenance. flakeRevision/nixpkgsRevision are NOT
            # currently plumbed into the chart eval (framework.nix passes
            # only `pkgs`/`lib`/`firestreamLib` to eval-chart.nix); Phase 1
            # leaves them as `null` defaults so the manifest stays valid.
            # TODO(provenance): plumb `inputs.self.rev` and
            # `inputs.nixpkgs.rev` from nix/flake-modules/framework.nix
            # through evalChart into these defaults so the manifest carries
            # real provenance. Until then, downstream agents should treat
            # null as "unknown".
            provenance = lib.mkOption {
              description = "Build-time provenance (flake/nixpkgs revs).";
              default = {};
              type = lib.types.submodule {
                options = {
                  flakeRevision = lib.mkOption {
                    type = lib.types.nullOr lib.types.str;
                    default = null;
                    description = "Firestream flake revision (inputs.self.rev). Null until plumbed (see TODO).";
                  };
                  nixpkgsRevision = lib.mkOption {
                    type = lib.types.nullOr lib.types.str;
                    default = null;
                    description = "Pinned nixpkgs revision. Null until plumbed (see TODO).";
                  };
                };
              };
            };
          };
        };
      };

      values = lib.mkOption {
        type = lib.types.attrsOf lib.types.anything;
        default = {};
        description = ''
          The serialised Helm values tree. Chart option modules write their
          typed options through to paths under here; to-values-yaml.nix strips
          null leaves and emits a faithful block values.yaml.
        '';
      };
    };
  };

  # Phase 6a: lift shared chart-agnostic types into evalChart's specialArgs so
  # per-chart option modules can write `{ lib, chartTypes, ... }: let t = chartTypes;
  # in { ... }` without counting `../` hops back to bin/nix/firestream/charts/lib/types/.
  # This is the canonical seam Agents G1–G7 will use for their per-chart overlays;
  # the shared types directory is the only chart-overlay-facing module surface
  # outside the chart's own `nix/options/` tree.
  chartTypes = import ./lib/types { inherit lib; };

  evaled = lib.evalModules {
    modules = [ (mkChartOptions { inherit name; }) ] ++ modules;
    specialArgs = { inherit pkgs lib chartTypes; };
  };

  cfg = evaled.config.${name};

  # The release-identity seam. `_meta.releaseName`/`_meta.namespace` are settable
  # options whose DEFAULTS are the `releaseName`/`namespace` function args (which
  # themselves default to `name`). Everything downstream — the render gate, the
  # deploy script, chart-manifest.json — must read the EVALUATED values, not the
  # closure-captured function args, or a consumer setting
  # `config.<name>._meta.namespace` would move the manifest but not the deploy.
  # No recursion risk: the option defaults depend on the args only, never on the
  # option itself.
  effectiveRelease   = cfg._meta.releaseName;
  effectiveNamespace = cfg._meta.namespace;

  # Namespace lifecycle. Read from the evaluated option for the same reason as
  # the two above: `bin/deploy` and chart-manifest.json must never disagree
  # about who creates the namespace, or the shell path and the Rust path
  # (helm_lifecycle/executor.rs) do different things from one bundle.
  createNamespaceFlag =
    lib.optionalString cfg._meta.createNamespace "--create-namespace";

  # Phase 2: merge container-registry-derived image overrides into the values
  # tree before serialisation. `cfg._meta.containerRefs` is populated by the
  # chart's flake-module from `firestreamImages.<name>` (see e.g.
  # nix/flake-modules/charts/airflow.nix). The injector is a pure function that
  # writes `{ registry; repository; tag; }` triples at each slot's
  # `componentPath`, with existing user values in `cfg.values` winning over
  # injected values at overlapping paths. Slots with `componentPath = []` are
  # recorded in chart-manifest.json but contribute no values overlay.
  injectContainerImages = import ./lib/inject-container-images.nix { inherit lib; };

  injectedValues = injectContainerImages {
    values = cfg.values;
    containerRefs = cfg._meta.containerRefs;
  };

  valuesYaml = (import ./lib/to-values-yaml.nix { inherit pkgs lib; }) injectedValues;

  # Extract chart version from Chart.yaml so the manifest carries the same
  # version Helm renders. Line-anchored regex matching `^version: <value>` (any
  # trailing whitespace tolerated). When Chart.yaml is absent or has no
  # `version:` line, fall back to the value baked into cfg if downstream
  # overrides it; otherwise the empty string.
  chartVersion =
    let
      chartYamlPath = chartSrc + "/Chart.yaml";
      lines =
        if builtins.pathExists chartYamlPath
        then lib.splitString "\n" (builtins.readFile chartYamlPath)
        else [];
      matchLine = line:
        let m = builtins.match "version:[[:space:]]+([^[:space:]]+)[[:space:]]*" line;
        in if m == null then null else builtins.head m;
      hits = builtins.filter (v: v != null) (map matchLine lines);
    in
      if hits == [] then "" else builtins.head hits;

  # Manifest paths point INSIDE the chart bundle derivation. Using
  # `builtins.placeholder "out"` would only resolve correctly if the manifest
  # were the bundle's own output; since the manifest is produced AS A SEPARATE
  # derivation and then copied into the bundle, we emit a sentinel string
  # (`@@BUNDLE_OUT@@`) and substitute it for the bundle's actual `$out` at
  # bundle-build time inside the runCommand below.
  manifestBundlePaths = {
    chartPath    = "@@BUNDLE_OUT@@/chart";
    valuesPath   = "@@BUNDLE_OUT@@/values.yaml";
    renderedPath = "@@BUNDLE_OUT@@/rendered.yaml";
  };

  chartManifestTemplate =
    (import ./lib/to-chart-manifest.nix { inherit pkgs lib; })
      cfg
      {
        inherit chartVersion;
        bundlePaths = manifestBundlePaths;
      };

  vendoredCharts = (import ./lib/vendor-subcharts.nix { inherit pkgs lib; }) {
    inherit subcharts;
  };

  # Filter heavy docs out of the chart source before it enters the store: each
  # of README.md / CHANGELOG.md in the airflow chart is hundreds of KB and plays
  # no role in rendering. Keep everything else verbatim (templates, values, etc).
  filteredChartSrc = lib.cleanSourceWith {
    src = lib.cleanSource chartSrc;
    name = "${name}-chart-src";
    filter = path: _type:
      let base = baseNameOf path;
      in base != "README.md" && base != "CHANGELOG.md";
  };

  # The airflow chart declares no kubeVersion constraint, so we omit
  # --kube-version. If a future chart adds one, we pass a satisfying value
  # (the cluster targets K8s 1.31, matching the k8s-openapi v1_31 feature flag).
  # Detection is line-anchored: builtins.match requires a full-string match and
  # `.` does not span newlines in Nix regex, so we test whether ANY line of
  # Chart.yaml begins with `kubeVersion:`.
  hasKubeVersion =
    let chartYaml = chartSrc + "/Chart.yaml";
    in builtins.pathExists chartYaml
       && lib.any
            (line: lib.hasPrefix "kubeVersion:" line)
            (lib.splitString "\n" (builtins.readFile chartYaml));

  kubeVersionFlag = lib.optionalString hasKubeVersion ''--kube-version "1.31.0"'';

  chartBundle = pkgs.runCommand "${name}-chart-bundle"
    {
      nativeBuildInputs = [ helm ];
    } ''
    set -euo pipefail
    mkdir -p "$out/chart" "$out/bin"

    # 1. Copy filtered chart source into $out/chart.
    cp -r --no-preserve=mode ${filteredChartSrc}/. "$out/chart/"

    # 2. Drop the vendored subcharts into the chart's charts/ dir.
    mkdir -p "$out/chart/charts"
    cp -r --no-preserve=mode ${vendoredCharts}/. "$out/chart/charts/"

    # 3. Copy the rendered values.yaml.
    cp ${valuesYaml} "$out/values.yaml"

    # 4. Remove Chart.lock — we never run `helm dependency`; a stale lock only
    #    produces an "out of sync" warning.
    rm -f "$out/chart/Chart.lock"

    # 5. Build-gate: render the chart fully offline. No --dependency-update.
    export HOME="$TMPDIR"
    helm template ${lib.escapeShellArg effectiveRelease} "$out/chart" \
      --namespace ${lib.escapeShellArg effectiveNamespace} \
      -f "$out/values.yaml" ${kubeVersionFlag} > "$out/rendered.yaml"

    # 6. Materialise chart-manifest.json. The template carries `@@BUNDLE_OUT@@`
    #    sentinels for `bundle.chartPath`/`valuesPath`/`renderedPath`; replace
    #    them with the bundle's actual `$out` so the manifest paths resolve
    #    against this very derivation's store path. We use plain `sed` (not
    #    `substituteAll`) because the value is `$out` itself, which mustn't be
    #    fixed at template-derivation build time.
    sed "s|@@BUNDLE_OUT@@|$out|g" ${chartManifestTemplate} > "$out/chart-manifest.json"

    # 7. Emit an executable deploy script.
    #    `--create-namespace` is present iff `_meta.createNamespace` (default
    #    true). A deploying layer that OWNS namespace lifecycle -- Pulumi, which
    #    also attaches the ResourceQuota, LimitRange, NetworkPolicy and Workload
    #    Identity ServiceAccount -- sets it false so Helm cannot race that by
    #    conjuring a bare, unquota'd namespace. The same option drives
    #    `release.createNamespace` in chart-manifest.json, so this script and the
    #    Rust deploy path never disagree.
    cat > "$out/bin/deploy" <<EOF
    #!${pkgs.runtimeShell}
    exec ${helm}/bin/helm upgrade --install ${lib.escapeShellArg effectiveRelease} \
      "$out/chart" \
      --namespace ${lib.escapeShellArg effectiveNamespace} ${createNamespaceFlag} \
      -f "$out/values.yaml" "\$@"
    EOF
    chmod +x "$out/bin/deploy"
  '';

in {
  inherit chartBundle valuesYaml;
  render = chartBundle + "/rendered.yaml";

  # The pre-substitution manifest (with @@BUNDLE_OUT@@ sentinels). Surfaced for
  # tests/inspection; consumers should read `${chartBundle}/chart-manifest.json`
  # which has the sentinels replaced with the bundle's `$out`.
  chartManifest = chartManifestTemplate;

  config = evaled.config;
  options = evaled.options;
}
