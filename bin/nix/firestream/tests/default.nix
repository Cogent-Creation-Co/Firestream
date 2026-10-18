{ pkgs, fenix, crane, system ? pkgs.system or "x86_64-linux" }:

let
  lib = pkgs.lib;
  # Firestream module needs fenix and crane for Rust builds
  firestream = import ../. {
    inherit pkgs system fenix crane;
    # Python inputs are optional, omit for tests
  };

  # Import individual test modules
  logTests = import ./test-log.nix { inherit pkgs firestream; };
  validationsTests = import ./test-validations.nix { inherit pkgs firestream; };
  fsTests = import ./test-fs.nix { inherit pkgs firestream; };
  osTests = import ./test-os.nix { inherit pkgs firestream; };
  netTests = import ./test-net.nix { inherit pkgs firestream; };
  serviceTests = import ./test-service.nix { inherit pkgs firestream; };
  fileTests = import ./test-file.nix { inherit pkgs firestream; };
  persistenceTests = import ./test-persistence.nix { inherit pkgs firestream; };
  integrationTests = import ./test-integration.nix { inherit pkgs firestream; };
  configTests = import ./test-config.nix { inherit pkgs firestream; };
  containerTests = import ./test-containers.nix { inherit pkgs firestream; };
  stateTests = import ./test-state.nix { inherit pkgs firestream; };
  sourceTests = import ./test-sources.nix { inherit pkgs firestream; };
  postgresqlEnvAliasTests = import ./test-postgresql-env-aliases.nix { inherit pkgs firestream; };
  seaweedfsChartValuesTests = import ./test-seaweedfs-chart-values.nix { inherit pkgs firestream; };
  pythonWorkspaceLibTests = import ./test-python-workspace-lib.nix { inherit pkgs firestream; };

in {
  # Individual test derivations
  inherit logTests validationsTests fsTests osTests netTests serviceTests fileTests persistenceTests integrationTests;
  inherit configTests containerTests stateTests sourceTests postgresqlEnvAliasTests seaweedfsChartValuesTests;
  inherit pythonWorkspaceLibTests;

  # All tests combined
  all = pkgs.runCommand "firestream-all-tests" {
    buildInputs = [
      logTests validationsTests fsTests osTests netTests serviceTests
      fileTests persistenceTests integrationTests configTests containerTests
      stateTests sourceTests postgresqlEnvAliasTests seaweedfsChartValuesTests
      pythonWorkspaceLibTests
    ];
  } ''
    echo "================================================"
    echo "All Firestream module tests passed!"
    echo "================================================"
    echo "Log tests:          PASSED"
    echo "Validations tests:  PASSED"
    echo "FS tests:           PASSED"
    echo "OS tests:           PASSED"
    echo "Net tests:          PASSED"
    echo "Service tests:      PASSED"
    echo "File tests:         PASSED"
    echo "Persistence tests:  PASSED"
    echo "Integration tests:  PASSED"
    echo "Config tests:       PASSED"
    echo "Container tests:    PASSED"
    echo "State tests:        PASSED"
    echo "Source tests:       PASSED"
    echo "PG env-alias tests: PASSED"
    echo "SeaweedFS chart:    PASSED"
    echo "Python workspace:   PASSED"
    echo "================================================"
    touch $out
  '';
}
