//! CLI surface tests: every flag the Python tool exposes is parseable here.

use assert_cmd::Command;
use predicates::str::contains;

#[test]
fn help_lists_all_python_flags() {
    let bin = env!("CARGO_BIN_EXE_firestream-nix-build");
    let out = Command::new(bin).arg("--help").assert().success();
    // Each predicate is one Python flag we must surface; if any of these
    // disappears, a CI call site (or a downstream script) silently breaks.
    let must_have = [
        "--flake",
        "--file",
        "--attr",
        "--arg",
        "--argstr",
        "--include",
        "--impure",
        "--pure",
        "--max-jobs",
        "--option",
        "--remote-ssh-option",
        "--cachix-cache",
        "--attic-cache",
        "--attic-ignore-upstream-cache-filter",
        "--attic-push-build-closure",
        "--niks3-server",
        "--no-nom",
        "--systems",
        "--retries",
        "--no-link",
        "--out-link",
        "--remote",
        "--always-upload-source",
        "--no-download",
        "--skip-cached",
        "--copy-to",
        "--debug",
        "--eval-max-memory-size",
        "--eval-workers",
        "--result-file",
        "--result-format",
        "--override-input",
        "--select",
        "--reference-lock-file",
        "--otel-ingest",
        "--otel-parent-trace",
        "--otel-service",
        "--otel-cli",
    ];
    let combined: String = String::from_utf8_lossy(&out.get_output().stdout).into_owned();
    for flag in must_have {
        assert!(
            combined.contains(flag),
            "--help missing flag {flag}\n--- got ---\n{combined}"
        );
    }
}

#[test]
fn rejects_attr_in_flake_mode() {
    let bin = env!("CARGO_BIN_EXE_firestream-nix-build");
    Command::new(bin)
        .args(["-A", "hello", "--flake", ".#checks"])
        // `--nix` invokes a subprocess; point at /bin/true so we fail at
        // validation rather than at `nix config show`.
        .env("NIX_FAST_BUILD_NIX", "/bin/true")
        .assert()
        .failure()
        .stderr(contains("only supported in non-flake"));
}
