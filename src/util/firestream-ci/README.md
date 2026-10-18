# firestream-ci

Build-pipeline primitives — a Nix + Docker + OTel build-orchestration toolkit
that composes the sibling `otel-cli` and `firestream-nix-build` crates into a
single library surface. Lineage: the patterns were extracted from ConceptDB's
5,868 LoC of CI bash under `bin/` and its 1,405-line `Makefile` (this crate was
lifted from ConceptDB's `src/util/oxi-ci`).

There is deliberately **no project-specific cargo feature**. Everything a
project needs to configure — phase attr lists, tier rules, export targets,
builder image identity, passthrough env vars, devshell sentinels — is runtime
data loaded from a `ci-manifest.json` profile. See `src/profile/` for the
reader and `bin/nix/firestream/ci/` for the typed Nix producer; the contract
mirrors this repo's Helm-chart contract (`CLAUDE.md`) one file for one file.

Profile resolution: `--profile <path>` -> `$FIRESTREAM_CI_PROFILE` ->
`./ci-manifest.json` -> `/opt/firestream/ci/ci-manifest.json`. Inspect what a
profile resolves to with `firestream-ci ci --dry-run` (prints the phase DAG,
tiers, expanded attrs, and export rules; runs nothing).

Two gates keep the core API project-agnostic: `tests/lift-fixture/` compiles an
external consumer against the crate with no project feature at all, and
`tests/profile_fixtures.rs` reproduces every assertion of the deleted
`defaults` module (preserved verbatim at `docs/defaults-reference.rs.txt`) from
a *synthetic non-Firestream* JSON profile alone.

## Module map

The 22 load-bearing patterns from the plan, one line per module:

| #     | Module        | Pattern                                                                   |
|-------|---------------|---------------------------------------------------------------------------|
| 1     | `trace`       | OTel root span, traceparent in/out default-on, checkpoint dir.            |
| 2     | `exec`        | Instrumented child: span, log capture, exit-code, OOM annotation.         |
| 3     | `reenter`     | "You must run inside container X / shell Y" re-exec guard.                |
| 4     | `runner`      | 4-way runner dispatch: host-linux / host-darwin / docker / cloudbuild.    |
| 5     | `devshell`    | Warm-bootstrap a Nix dev shell with stale-source pruning.                 |
| 6/7/8/9 | `oci`       | Image resolve fallback, pull retry, flatten safety pipeline, source sync. |
| 10    | `worktree`    | Worktree → libgit2-valid mount chain with symlink-divergence walk.        |
| 11    | `pipeline`    | Phase DAG with Tier (Required/Advisory), parallel-within-phase, Verdict.  |
| 12/13/14 | `nix`      | Typed `FastBuild` builder, span reconcile, JSON-after-success synth.      |
| 15    | `rundir`      | Per-run output dir: epoch-partitioned, age-pruned, sentinel-sync.         |
| 16    | `manifest`    | `_build/<run>/manifest.json` artifact manifest — stable downstream contract. |
| 17    | `passthrough` | Typed env-var allowlist; emit `--env` / `Environment=` lists.             |
| 18    | `checkpoint`  | Lock + scan + emit + rename loop; thin wrapper over `otel_cli::checkpoint`. |
| 19    | `report`      | Span tree → phase table / build table / markdown summary.                 |
| 20    | `version`     | Multi-format file manifest: check + atomic set with pluggable `FileFormat`. |
| 21    | `limits`      | `LimitedCommand`: cgroup (Linux), watchdog (Darwin), unified API.         |
| 22    | `service`     | Cross-platform service unit installer: launchd plist, systemd user timer. |
| 23    | `util`        | GNU `timeout` / `gtimeout` / passthrough shim, misc helpers.              |
| —     | `profile`     | The CI profile: `ci-manifest.json` (schema v1) reader — phases, tiers, export targets, builder identity, passthrough set, devshell sentinels. Everything that used to be a project-specific cargo feature. |

## Status

Skeleton; modules unimplemented until Phase 4+. The Phase 3 deliverables are
the crate structure, validation gates (lift-fixture, MSRV check, no-leaf-cycle
test), workspace integration, and Nix wiring — module bodies are stubs.
