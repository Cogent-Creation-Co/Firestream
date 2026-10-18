# Scope: migrating `oxi-ci` into Firestream

Status: **implemented.** Retained as the historical scoping record — it is the
"why", written before any code existed. The shipped shape is `src/util/`
(`firestream-ci`, `firestream-otel-cli`, `firestream-nix-build`), driven by
`make ci` / `make ci-check`; where this document and the tree disagree, the
tree wins.

Sources inspected (the ConceptDB-dev checkout, a sibling repo — not vendored here):
- `<ConceptDB-dev>/src/util/{oxi-ci,otel-cli,oxi-nix-fast-build}`
- `<ConceptDB-dev>/docs/08-operations/ci-cd.md`
- This repo: `makefile`, `flake.nix`, `nix/flake-modules/**`, `bin/build/**`, `src/lib/rust/firestream-e2e-{core,k8s}`

---

## 1. What `oxi-ci` actually is

Not a CI *service*. It is a **Rust library + single binary that replaces CI bash**, built by
extracting 22 named patterns from ~5,900 LoC of ConceptDB CI bash and a 1,405-line Makefile.
Despite the README saying "skeleton", the modules are implemented.

| Crate | LoC (`*.rs`, excl. target) | Role |
|---|---|---|
| `oxi-ci` | 19,912 (4,764 of which is the CLI) | orchestration library + `oxi-ci` binary |
| `otel-cli` | 13,207 | Rust port of equinix-labs/otel-cli; OTLP gRPC/HTTP/**json+file** sinks |
| `oxi-nix-fast-build` | 3,506 | Rust port of `nix-fast-build` with in-process OTel ingest |
| **total** | **~36,600** | isolated virtual Cargo workspace at `src/util/` |

Module map (each mirrors a specific bash function, cited in its doc header):

| Module | What it gives you |
|---|---|
| `trace` | OTel root span, `TRACEPARENT` in/out, RAII spans, durable disk emission |
| `exec` | instrumented child process: span + per-stream log capture + exit code + OOM/signal annotation |
| `reenter` | "must run inside container/devshell" re-exec guard, preserving `TRACEPARENT` + env allowlist |
| `runner` | 4-way dispatch: `host-linux` / `host-darwin` / `docker` / `cloudbuild`, sentinel auto-detect |
| `devshell` | warm-bootstrap a Nix devshell from a cached `print-dev-env`, stale-source prune, git compaction |
| `oci` | image resolve fallback chain, pull retry, `export\|import` flatten pipeline with size/lineage guards, `git ls-files` source sync |
| `worktree` | worktree → libgit2-valid docker mount chain via symlink-divergence walk |
| `pipeline` | phase DAG, `Tier::{Required,Advisory}`, parallel-within-phase, `Verdict` → exit 0/1/2 |
| `nix` | typed `FastBuild` builder over `oxi-nix-fast-build`, span reconcile vs result JSON, failure synthesis |
| `rundir` | `_build/<date>/<sha>/<epoch>/{spans,logs,profiles,artifacts}`, age prune, host↔container sentinel handshake |
| `manifest` | `manifest.json` schema v2 — NDJSON-streamed entries, atomic finalize; the stable downstream contract |
| `artifacts` | materialize Nix store outputs into the rundir with sha256 + size + copy/hardlink/symlink modes + size cap |
| `passthrough` | typed env allowlist rendered three ways (docker `--env`, systemd `Environment=`, exec kv) |
| `checkpoint` | flock + scan + emit + rename replay loop for orphaned background spans; Honeycomb target |
| `report` | span tree → phase table / build table / markdown summary |
| `dashboard` | live `superconsole` UI: per-task spinners, tail-N of live build output, failure log dumps into scroll history |
| `limits` | `LimitedCommand`: systemd-run cgroup caps (Linux) / polling RSS watchdog (Darwin) |
| `version` | multi-format version manifest (Cargo.toml / package.json / pyproject.toml) with all-or-nothing atomic set |
| `service` | launchd plist / systemd user timer installer |
| `wire` | protobuf-defined NDJSON-over-stdio "agent mode" event stream |
| `defaults` | **feature-gated** ConceptDB specifics (attr lists, image names, passthrough vars, tier classifier) |

CLI surface today: `spans replay`, `version check`, `worktree mounts`, `limits exec|watchdog`,
`oci flatten`, `report summary`, `nix gc`, `k8s namespace`, `sweep`, and the four orchestrators
`ci`, `ci-linux`, `ci-docker`, `ci-darwin`, `ci-cloudbuild`.

**It was designed to be lifted.** `defaults` is behind the `conceptdb` cargo feature, and
`tests/lift_fixture_builds.rs` is a CI gate that `cargo check`s a synthetic external consumer
with default features off — any ConceptDB coupling leaking into the core API fails the test.
The `wire` module is deliberately self-contained (no core-types dependency) for the same reason.
This is the single most important fact for scoping: the extraction cost is low **by construction**.

---

## 2. Firestream's build reality today

- **There is no CI.** `CLAUDE.md` documents `.github/workflows/build.yaml`, but `git ls-files
  .github` is empty on `nightly` — the only tracked workflows belong to the vendored Bitnami chart
  fork. The `make test` / `make build` targets are literally `cargo test --workspace` /
  `cargo build --workspace`.
- **The expensive surface is entirely unautomated and unobserved.** 10 container images
  (`nix/flake-modules/containers/*`), 10 charts (`nix/flake-modules/charts/*`), 13 flake checks,
  a fleet SBOM manifest, plus two e2e harnesses. Both e2e harnesses are `#[ignore]`d, serialized
  `--test-threads=1`, local-only, and documented as "cold runs are hours" and explicitly
  out of CI.
- **The build orchestration is bash**, ~3,150 LoC under `bin/` + `docker/`. Critically,
  `bin/build/_common.sh` and `bin/build/manifest.sh` implement *the same patterns* oxi-ci
  typed: repo-root detection through worktrees, git-worktree docker mount resolution,
  physical-path resolution, docker resource detection, persistent per-arch Nix store volume.
  This is shared lineage with ConceptDB's `bin/build/_common.sh` — oxi-ci's `worktree`,
  `rundir`, and `manifest` modules are the finished version of the scripts Firestream still runs.
- **No observability of any kind.** Zero `opentelemetry`/`otlp` references in the workspace.
  A multi-hour image+chart build emits nothing but terminal scrollback.
- **No parallel Nix scheduler.** Building 10 images is 10 sequential `nix build` invocations
  with no cross-derivation queueing, no per-attribute failure isolation, no result JSON.
- **Partial overlap already exists**: `firestream-e2e-core` has `exec.rs`, `probe.rs`, `retry.rs`,
  `guard.rs`. These are e2e-scoped (trait `Exec` over docker-compose/kubectl targets), not
  process-orchestration-scoped, so they complement rather than duplicate oxi-ci's `exec`.

---

## 3. What Firestream gains — ranked

### Tier 1 — the reason to do this at all

1. **`nix` + `oxi-nix-fast-build`: parallel, per-attribute, observable Nix builds.**
   Firestream's 10 images and 10 charts become one `FastBuild` invocation with a bounded job
   queue, per-attribute success/failure records in result JSON, and spans reconciled against
   them. Today a single failing image kills the sequential loop and you re-run the whole thing.
   This is the largest single wall-clock and diagnosability win available to this repo.

2. **`pipeline`: Required/Advisory tiers with a real verdict.**
   Firestream has a natural tier split it currently cannot express: flake checks and container
   builds are Required; SBOM/manifest generation, chart parity checks, and the e2e sweeps are
   Advisory. Exit 2 (PartiallyPassed) is exactly the semantics needed to put the e2e harnesses
   *in* CI without them blocking merges — which is the stated reason they're excluded today.

3. **`trace` + `report` + `checkpoint`: build observability from zero.**
   The `json+file` OTLP sink in `otel-cli` means this works with **no collector** — spans land
   as files in the rundir, `oxi-ci report summary` renders phase/build tables and a markdown
   summary. For a build measured in hours, "which derivation ate 40 minutes" is currently
   unanswerable and becomes a table.

4. **`rundir` + `manifest` + `artifacts`: a stable build-output contract.**
   Firestream already has `packages.manifest` (fleet SBOM) and `bin/build/manifest.sh`, but no
   per-run directory, no artifact materialization out of the store, no sha256/size records, and
   no GC-safety. oxi-ci's schema-v2 `manifest.json` plus the artifacts copier is a drop-in
   upgrade, and the host↔container `.sentinel` handshake directly hardens the
   Docker-from-Docker mount pattern this repo is built on.

### Tier 2 — high value, direct replacement of existing Firestream bash

5. **`worktree`**: replaces `resolve_git_mounts` in `bin/build/_common.sh`. Same problem
   (libgit2 + symlinked worktrees + docker bind mounts), typed and unit-tested.
6. **`oci` flatten + source sync**: Firestream's `bin/build/container-images.sh` (313 LoC) does
   the Docker-in-container Nix build with a persistent store volume. The flatten pipeline gives
   it warm-cache image lineage with size/dangling-image guards; `source_sync`'s
   tracked+untracked `git ls-files` union is what makes the in-container Nix source hash match a
   host `nix build` — a determinism property Firestream currently doesn't guarantee.
7. **`runner` + `reenter`**: formalizes the thing `bootstrap.sh` already does by hand
   ("detects if running outside Docker and auto-launches via `docker compose`"), and adds
   Darwin as a first-class runner — which matters because `docker-build.nix` exists precisely to
   let Darwin hosts build Linux images.
8. **`limits`**: the devcontainer minimums are 4 CPU / 8 GB. A runaway Spark/Airflow image build
   OOMs the host. cgroup `MemoryMax` + `IOWriteBandwidthMax` on Linux, RSS watchdog on Darwin.
9. **`dashboard`**: the e2e harnesses today rely on `--nocapture --test-threads=1` for readable
   output. A live per-task view with tail-N and failure-log dumps is strictly better and removes
   the reason for serializing.

### Tier 3 — useful, low priority

10. `sweep` (repo-local `target/` + `_build/` hygiene) — Firestream has `docker-reset`,
    `nix-fix`, `builder-cache-clean` doing adjacent things by hand.
11. `version` — Firestream has Cargo.toml + pyproject.toml + package.json version surfaces
    (uv, pnpm, cargo) with no cross-file consistency check.
12. `service` — only if a span-replay daemon is wanted.
13. `passthrough` — small, but it is the correct home for the `DEPLOYMENT_MODE` / `MACHINE_ID` /
    `GIT_COMMIT_HASH` / `CPU_ARCHITECTURE` / `HAS_NVIDIA_GPU` env contract `bootstrap.sh` defines.

### What does *not* transfer

- `defaults/conceptdb` — attr lists, `conceptdb-builder` image, GPU server variants. Replaced
  wholesale by a `firestream` feature module.
- `ci-darwin`'s Xcode/xcodegen/iOS phases — Firestream has no Apple app targets. The Darwin
  runner shell is still useful (Darwin-host Linux image builds); the iOS body is dead weight.
- `ci-cloudbuild` — thin GCP Cloud Build policy bundle. Firestream targets GitHub Actions;
  keep the module as a template for a future GKE path, don't wire it.
- `k8s namespace` (branch → namespace mapping) — ConceptDB-shaped, but Firestream's k3d/k8s
  e2e could adopt the same idea for parallel-safe test namespaces.

---

## 4. Recommended integration shape

**Vendor as an isolated virtual Cargo workspace at `src/util/`, mirroring ConceptDB exactly.**

Do **not** add these crates to the root Firestream workspace. Reasons:
- Firestream crates are `edition = "2024"`; the `src/util` workspace is `edition 2021`,
  `rust-version = 1.82`. Mixed editions are legal per-crate, but merging pulls ~36k LoC and a
  large transitive graph (tonic, axum ×2, reqwest, rustls, bollard, git2, superconsole, ratatui)
  into the root lockfile, where it would fight Firestream's own pins and slow every
  `cargo build --workspace`.
- ConceptDB's migration notes already document 15+ accepted duplicate transitive majors in
  that lockfile. Importing that into Firestream's root is a self-inflicted wound.
- Isolation is the arrangement that has been proven to work in the source repo.

Consequence: `cargo test --workspace` at the Firestream root does **not** cover `src/util`;
it needs its own make target and its own flake package (crane, mirroring how ConceptDB wires it).

Rejected alternatives:
- *Flake input from ConceptDB* — creates a hard dependency on a private sibling repo and makes
  the `firestream` defaults module homeless. Reconsider only if oxi-ci is published standalone.
- *Fork-and-diverge* — guarantees drift. Prefer a clean vendor with the `conceptdb` feature
  deleted and a `firestream` feature added, then upstream generic fixes back to ConceptDB by hand.

---

## 5. Phasing

Each phase is independently shippable and leaves the tree green.

**Phase 0 — decide + prove the lift (~0.5 day).**
Copy the three crates to `Firestream/src/util/`, delete `defaults/conceptdb`, build with
`--no-default-features`, and make the lift-fixture test pass in-tree. This is the honest cost
probe: if it doesn't build clean with defaults off, everything below re-prices.

**Phase 1 — Nix packaging + workspace wiring (~1 day).**
`nix/flake-modules/util.nix` producing `packages.{oxi-ci,otel-cli,oxi-nix-fast-build}` via crane
(Firestream already has crane + fenix as flake inputs). Add to the devshell. Add
`make test-util` / `make build-util`. No behavior change to anything existing.

**Phase 2 — `defaults/firestream` (~1–2 days).**
The new feature module: check attr lists (the 13 flake checks), image attr lists (the 10
containers), chart attr lists (the 10 charts), tier classifier, env passthrough set from
`bootstrap.sh`, artifact export targets for the fleet manifest and per-container SBOMs.
This is where all Firestream-specific knowledge lives and is the main new authorship.

**Phase 3 — `ci-linux` orchestrator, advisory-only (~2–3 days).**
Wire `pipeline` phases: `verify` (flake checks, Required) → `build` (containers + charts via
`FastBuild`, Required) → `attest` (fleet manifest + SBOM presence, Advisory). Run it locally
alongside the existing bash. Turn on `trace` with the `json+file` sink and `rundir`/`manifest`.
First point at which "how long does a Firestream build take, and where" becomes answerable.

**Phase 4 — replace `bin/build/*.sh` (~2–3 days).**
`container-images.sh` → `oci` + `worktree` + `runner`; `manifest.sh` → `manifest` + `artifacts`;
`_common.sh` mount/root/resource logic → the typed equivalents. Delete the bash once the Rust
path is proven on both Linux and Darwin hosts. This is the "deeply integrated" milestone —
after it, the build system *is* oxi-ci.

**Phase 5 — GitHub Actions (~1 day).**
Author the workflow that does not currently exist, entry point `oxi-ci ci`. Required phases gate
PRs; Advisory phases report. The 45-minute-timeout / space-cleanup constraints described in
`CLAUDE.md` are what `limits` and `sweep` are for.

**Phase 6 — fold the e2e harnesses in (~2 days).**
`firestream-e2e-k8s` and the docker `e2e.rs` become Advisory pipeline phases with per-chart
tasks running in parallel under the dashboard, instead of `#[ignore]`d serialized cargo tests.
Keeps them out of the merge gate while making them run and report continuously. Requires
reconciling `firestream-e2e-core::Exec` with `oxi_ci::exec` — the recommendation is to keep both
and have the e2e harness spawn *through* `oxi_ci::exec` for span/log capture.

**Optional Phase 7 — collector.** Point `OTEL_EXPORTER_OTLP_ENDPOINT` at something real (or
install the `service` replay timer). Not required; `json+file` + `report summary` covers the
local case fully.

Rough total: **10–14 engineering days** to Phase 5, ~2 more for Phase 6. Phases 0–2 are
mechanical; Phase 3–4 carry the risk.

---

## 6. Risks

| Risk | Assessment |
|---|---|
| Hidden ConceptDB coupling | **Low** — the lift-fixture gate exists precisely to prevent it. Phase 0 confirms empirically in half a day. |
| `oxi-nix-fast-build` parity | **Medium** — ConceptDB's own README says CI still calls the vendored *Python* `nix-fast-build`; the Rust port had not been cut over at the time of writing. Verify current status in ConceptDB before betting Phase 3 on it, and keep a plain-`nix build` fallback path. |
| Dep-graph weight | **Low if isolated**, high if merged into the root workspace. Mitigated by the `src/util` decision. |
| Two divergent copies | **Medium and permanent.** No shared-crate mechanism is proposed. Accept drift, or plan to publish oxi-ci to a registry / its own repo later. Worth deciding now rather than after Phase 4. |
| Darwin path | **Medium** — `ci-darwin` is heavily Xcode-shaped; Firestream needs a different Darwin body (Linux-image-build-via-Docker). Budget for rewrite, not adaptation. |
| Build times get *worse* first | Real — parallel Nix builds on a 4-CPU/8 GB devcontainer will thrash without `limits` tuned. Phase 3 should land `limits` at the same time as `FastBuild`. |

---

## 7. Open questions for the user

1. Is ConceptDB's `oxi-nix-fast-build` cut over from the Python `nix-fast-build` yet? Phase 3's
   value depends on it.
2. One-way vendor (accept drift) or is a shared upstream (own repo / registry publish) wanted?
   This decision is cheap now and expensive after Phase 4.
3. Is GitHub Actions the CI target, or is a GKE/Cloud Build path planned (which would make
   `ci-cloudbuild` worth keeping)?
4. Should the e2e harnesses become pipeline phases (Phase 6), or stay cargo tests invoked *by* a
   pipeline phase? The former is more integrated; the latter preserves `cargo test` ergonomics.
