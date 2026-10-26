# docker-compose generator flake-module
# Copyright Firestream. MIT License.
#
# Renders a deployable docker-compose.yml for every container from its evaluated
# `compose` config (eval-container.nix), so the image tags in the compose file
# always match what the flake builds. Contributes (on EVERY system):
#   - packages.<name>-compose   derivation holding docker-compose.yml (+ README)
#                               .passthru: { composeFile; projectName; hostPorts;
#                                            portMap; healthHostPort; buildList; }
#   - apps.<name>-up            build+load required images, then `docker compose up -d`
#   - apps.<name>-down          `docker compose down`
#   - checks.firestream-port-registry  the host-port registry rules
#   - legacyPackages.portRegistry      { entries; markdownTable; }
#
# Every host port comes from bin/nix/firestream/ports.nix.
#
# Both apps take the compose project name from FIRESTREAM_COMPOSE_PROJECT when
# it is set, else from `compose.projectName`. The e2e harness sets it so its
# runs never share a project, a volume or a network with a dev stack.
#
# Iterating config.firestreamImages (NOT the isLinux-gated firestreamContainers)
# keeps this working on Darwin. Reading `.imageRef` / `.compose` / `.exposedPorts`
# only forces option evaluation — never the Linux-only image build.
{ ... }: {
  perSystem = { pkgs, lib, config, firestreamBuildImage, ... }:
    let
      yamlFormat = pkgs.formats.yaml { };

      # Re-evaluate a registry image with no overrides to read its config.
      evalImage = name: config.firestreamImages.${name}.eval (_: { });
      depImageRef = key: (evalImage key).imageRef;

      # null ⇒ own image; "@key" ⇒ dependency image ref (tag stays in sync); else literal.
      resolveImage = ownRef: img:
        if img == null then ownRef
        else if lib.hasPrefix "@" img then depImageRef (lib.removePrefix "@" img)
        else img;

      portsLib = import ../../bin/nix/firestream/ports-lib.nix { inherit lib; };
      portRegistry = portsLib.registry;

      portEntry = hostPort: container: "${toString (hostPort container)}:${toString container}";

      mkService = { ownRef, sharedEnv, hostPort }: svc:
        let
          isOwn = svc.image == null;
          envMerged = (if isOwn then sharedEnv else { }) // svc.env;
          ports = map (portEntry hostPort) svc.ports;
        in
        { image = resolveImage ownRef svc.image; }
        // lib.optionalAttrs (envMerged != { }) { environment = envMerged; }
        // lib.optionalAttrs (ports != [ ]) { inherit ports; }
        // lib.optionalAttrs (svc.volumes != [ ]) { volumes = svc.volumes; }
        // lib.optionalAttrs (svc.command != null) { inherit (svc) command; }
        // lib.optionalAttrs (svc.dependsOn != [ ] || svc.dependsOnCompleted != [ ]) {
          depends_on = lib.listToAttrs
            (map (d: { name = d; value = { condition = "service_healthy"; }; }) svc.dependsOn
              ++ map (d: { name = d; value = { condition = "service_completed_successfully"; }; }) svc.dependsOnCompleted);
        }
        // lib.optionalAttrs (svc.healthcheck != null) { healthcheck = svc.healthcheck; }
        // lib.optionalAttrs (svc.restart != "") { restart = svc.restart; };

      # Per-container compose model + rendered YAML.
      mkCompose = name:
        let
          c = evalImage name;
          cmp = builtins.seq c.compose.hostPortOffset c.compose;
          ownRef = c.imageRef;
          stack = portRegistry.stackOf name;

          # Phase 4: synthesise a compose healthcheck for the default
          # single-service branch when the container opts into the in-image
          # firestream-healthd (`health.enable = true`). One contract,
          # three consumers (Rust e2e harness, compose `service_healthy`,
          # future k8s probes). The bash /dev/tcp pattern is universal:
          # every firestream image bundles bashInteractive, so we never need
          # wget/curl/etc. in PATH — just bash.
          defaultHealthcheck = lib.optionalAttrs (c.health.enable or false) {
            healthcheck = {
              test = [
                "CMD"
                "bash"
                "-c"
                "exec 3<>/dev/tcp/127.0.0.1/${toString c.health.port} && printf 'GET /readyz HTTP/1.0\\r\\n\\r\\n' >&3 && head -n 1 <&3 | grep -q ' 200'"
              ];
              interval = "10s";
              timeout = "5s";
              retries = 5;
              start_period = "30s";
            };
          };

          # NOTE: the single service of a one-service stack takes the stack's
          # name in the registry, so the version variants of an image share
          # the rows of their base stack.
          publishedPorts =
            if cmp.services == { } then
              [{ yamlService = name; service = stack; containers = c.exposedPorts; own = true; }]
            else
              lib.mapAttrsToList
                (svcName: svc: { yamlService = svcName; service = svcName; containers = svc.ports; own = svc.image == null; })
                cmp.services;

          hostPortOf = service: container:
            portRegistry.hostPort { inherit stack service container; };

          services =
            if cmp.services == { } then
              {
                "${name}" = { image = ownRef; }
                  // lib.optionalAttrs (c.exposedPorts != [ ]) {
                  ports = map (portEntry (hostPortOf stack)) c.exposedPorts;
                }
                  // defaultHealthcheck;
              }
            else
              lib.mapAttrs
                (svcName: mkService {
                  inherit ownRef;
                  sharedEnv = cmp.sharedEnv;
                  hostPort = hostPortOf svcName;
                })
                cmp.services;

          hostPorts = lib.concatMap (p: map (hostPortOf p.service) p.containers) publishedPorts;

          # { <compose service> = { "<container port>" = <host port>; }; }
          portMap = lib.listToAttrs (map
            (p: lib.nameValuePair p.yamlService
              (lib.listToAttrs (map (cp: lib.nameValuePair (toString cp) (hostPortOf p.service cp)) p.containers)))
            (lib.filter (p: p.containers != [ ]) publishedPorts));

          published = lib.concatMap
            (p: map (container: { inherit stack container; inherit (p) service; }) p.containers)
            publishedPorts;

          # NOTE: the healthd port of the stack is the one that an own-image
          # service publishes; dependency images such as postgresql never
          # publish theirs here.
          healthHostPorts = lib.concatMap
            (p: lib.optional (p.own && lib.elem c.health.port p.containers) (hostPortOf p.service c.health.port))
            publishedPorts;
          healthHostPort =
            if !(c.health.enable or false) || healthHostPorts == [ ] then null
            else lib.head healthHostPorts;

          composeData = { name = cmp.projectName; inherit services; }
            // lib.optionalAttrs (cmp.volumes != { }) { volumes = cmp.volumes; };
        in
        {
          inherit cmp ownRef hostPorts portMap published healthHostPort;
          composeYaml = yamlFormat.generate "${name}-docker-compose.yml" composeData;
          buildList = [ name ] ++ cmp.dependencies;
        };

      mkComposePkg = name:
        let
          g = mkCompose name;
        in
        if g.cmp.enable then
          pkgs.runCommand "${name}-compose"
            {
              passthru = {
                composeFile = g.composeYaml;
                projectName = g.cmp.projectName;
                buildList = g.buildList;
                inherit (g) hostPorts portMap healthHostPort;
              };
            }
            ''
              mkdir -p "$out"
              cp ${g.composeYaml} "$out/docker-compose.yml"
              cat > "$out/README.md" <<EOF
              # ${name} — Firestream docker-compose

              Project: ${g.cmp.projectName}
              Images:  ${toString g.buildList}

              Build + load the images and start the stack:

                  nix run .#${name}-up

              Or manually:

                  ${lib.concatMapStringsSep "\n    " (p: "nix run .#${p}-image -- --load") g.buildList}
                  docker compose -f \$out/docker-compose.yml -p ${g.cmp.projectName} up -d

              Tear down:

                  nix run .#${name}-down
              EOF
            ''
        else
          pkgs.runCommand "${name}-compose-disabled" { } ''
            echo "compose generation is disabled for ${name}" > "$out"
          '';

      mkUpApp = name:
        let
          g = mkCompose name;
          composePkg = mkComposePkg name;
          buildSteps = lib.concatMapStringsSep "\n"
            (p: ''"${firestreamBuildImage}/bin/firestream-build-image" ${lib.escapeShellArg p} --load'')
            g.buildList;
        in
        {
          type = "app";
          program = "${pkgs.writeShellScriptBin "firestream-${name}-up" ''
            set -euo pipefail
            echo ">>> Building + loading images: ${toString g.buildList}" >&2
            ${buildSteps}
            exec docker compose -f ${composePkg}/docker-compose.yml -p "''${FIRESTREAM_COMPOSE_PROJECT:-${g.cmp.projectName}}" up -d "$@"
          ''}/bin/firestream-${name}-up";
        };

      mkDownApp = name:
        let
          g = mkCompose name;
          composePkg = mkComposePkg name;
        in
        {
          type = "app";
          program = "${pkgs.writeShellScriptBin "firestream-${name}-down" ''
            set -euo pipefail
            exec docker compose -f ${composePkg}/docker-compose.yml -p "''${FIRESTREAM_COMPOSE_PROJECT:-${g.cmp.projectName}}" down "$@"
          ''}/bin/firestream-${name}-down";
        };

      names = builtins.attrNames config.firestreamImages;

      portsDocPath = ../../docs/ports.md;

      portRegistryCheck =
        let
          composeNames = lib.filter (n: (evalImage n).compose.enable) names;
          covered = portRegistry.checkCoverage {
            published = lib.concatMap (n: (mkCompose n).published) composeNames;
            imageKeys = names;
          };
          docInSync = portRegistry.tableIn (builtins.readFile portsDocPath) == portRegistry.markdownTable
            || throw ''
              docs/ports.md: the table between the port-table markers differs from
              bin/nix/firestream/ports.nix. Put this table there:

              ${portRegistry.markdownTable}'';

          # NOTE: each rule must still reject a broken table; a rule that
          # silently passes everything would keep this check green.
          row = stack: service: container: host: [ stack service container host ];
          errorsOf = rows: portsLib.registryErrors { inherit rows; stackOf = { }; };
          small = portsLib.mkRegistry { rows = [ (row "a" "a" 80 11080) ]; stackOf = { }; };
          rulesReject = lib.all (x: x) [
            (errorsOf [ (row "a" "a" 80 11080) (row "b" "b" 80 11080) ] != [ ])
            (errorsOf [ (row "a" "a" 80 11080) (row "a" "a" 80 11081) ] != [ ])
            (errorsOf [ (row "a" "a" 80 1023) ] != [ ])
            (errorsOf [ (row "a" "a" 80 32768) ] != [ ])
            (errorsOf [ [ "a" 80 11080 ] ] != [ ])
            (portsLib.registryErrors { rows = [ (row "a" "a" 80 11080) ]; stackOf.x = "missing"; } != [ ])
            (!(builtins.tryEval (small.hostPort { stack = "a"; service = "a"; container = 81; })).success)
            (small.coverageErrors { published = [ ]; imageKeys = [ "a" ]; } != [ ])
            (small.coverageErrors { published = [ { stack = "a"; service = "a"; container = 80; } { stack = "a"; service = "a"; container = 81; } ]; imageKeys = [ "a" ]; } != [ ])
            (small.coverageErrors { published = [ { stack = "a"; service = "a"; container = 80; } ]; imageKeys = [ "a" ]; } == [ ])
          ] || throw "firestream-port-registry: a registry rule accepts a table that it must reject";
        in
        assert covered && docInSync && rulesReject;
        pkgs.writeText "firestream-port-registry.md" portRegistry.markdownTable;
    in
    {
      checks.firestream-port-registry = portRegistryCheck;

      legacyPackages.portRegistry = {
        inherit (portRegistry) entries markdownTable;
      };

      packages = lib.listToAttrs
        (map (n: { name = "${n}-compose"; value = mkComposePkg n; }) names);

      apps =
        lib.listToAttrs (map (n: { name = "${n}-up"; value = mkUpApp n; }) names)
        // lib.listToAttrs (map (n: { name = "${n}-down"; value = mkDownApp n; }) names);
    };
}
