# Native-first Nix builds for Firestream

*Design record. The change described here is implemented — `bin/build/strategy.sh`,
`nix/flake-modules/docker-build.nix`, and the `nix-container-builder` fixes are on `nightly`.
Kept for the rationale and for the verification matrix, which still runs as written.*

## Context

Firestream builds its container images with Nix (`pkgs.dockerTools.buildLayeredImage`, via
`bin/nix/firestream/containers/base.nix:321`), but every shell and flake entry point runs that
build **inside** a `nixos/nix:latest` Docker container with a persistent per-arch Docker volume
mounted at `/nix`. That volume, `firestream-nix-store-<arch>`, is unbounded and unshared with the
host store — on the machine that prompted this work it had reached ~119 GB while being mounted by
nothing.

The Docker builder exists for one reason, stated plainly in `nix/flake-modules/docker-build.nix:4-9`:
the image derivations are gated `if isLinux then ... else unavailable "<pkg>"`, so `nix build .#airflow`
yields a stub on macOS. `containers.md:1-9` adds the second motive — "make Docker the only dependency,"
i.e. don't require the developer to have Nix.

**Neither premise holds on this host.** It is NixOS x86_64-linux, `nix` 2.34.7, flakes enabled globally,
`justin` in `trusted-users`, `/nix/store` already populated. The `isLinux` gate passes and the flake
instantiates natively — verified:
`nix eval --raw .#packages.x86_64-linux.postgresql-17.drvPath` → `/nix/store/w2k224nlz…-firestream-postgresql.tar.gz.drv`.

So on Linux the Docker builder is pure overhead: a second 118 GB Nix store that duplicates the host's,
a container start per build, cold-cache reads through a volume driver, and a live failure mode — the
sibling branch already had to patch a dangling `/etc/nix/nix.conf` symlink caused by the persistent
volume desyncing from a freshly pulled builder image (`docker-build.nix:147` on
`one-flake-to-rule-them-all`).

**Outcome:** builds use the host's `/nix/store` by default and fall back to Docker only when native is
genuinely impossible — macOS host, cross-arch target, no `nix` on PATH, or running inside a container.

**Reuse note:** `src/lib/rust/nix-container-builder/` already implements exactly this policy
(`platform.rs:129-167` `can_build_native()`, `strategy/mod.rs:200-227`), and `firestream build` is
already native-first here. The shell and flake paths simply never adopted it. So the predicate was
mirrored in shell rather than reinvented, and four real defects in the Rust crate were repaired
separately.

---

## Design

One new **side-effect-free** shell library, `bin/build/strategy.sh`, holds the entire decision predicate
and both build primitives. It is sourced by:

- `bin/build/_common.sh` — so `container-images.sh` and `manifest.sh` inherit it, and
- `nix/flake-modules/docker-build.nix` via `source ${../../bin/build/strategy.sh}`, which Nix
  substitutes as a **store path** — so it works in the `/nix/store` snapshot case where there is no
  repo checkout (see `SELF_STORE_PATH` at `docker-build.nix:31` and the `/nix/store/*` branch at `:113-119`).

That gives a single source of truth across all three shell/Nix consumers with **no dependency on the
Rust binary** — important, because making `nix run .#airflow-image` compile the Rust workspace would be
unacceptable for an external consumer's `inputs.firestream`.

**Explicitly not doing:** repointing `makefile:109`'s `BUILD_CONTAINER` at `firestream build`. I verified
the landmine: `nix/flake-modules/containers/redis.nix:7-10` aliases `.#redis` to **redis-8**, while
`_common.sh:166` maps bare `redis` to **redis-7**. Repointing would silently change Redis versions and
send `redis-7-start` into a rebuild loop. The Rust CLI also lacks `--version`, `--target`, and the batch lock.

---

## Phase 1 — shell + flake native path

### 1. New file `bin/build/strategy.sh`

Pure function definitions. **No `set -e`, no side effects, no dependency on `REPO_ROOT` or the `log_*`
helpers** — `docker-build.nix` sources it with none of those present. Provide guarded fallbacks:
`declare -F log_step >/dev/null || log_step() { printf '>>> %s\n' "$*" >&2; }`.

Move here from `_common.sh` (deleting them there): `resolve_physical` (`:17-28`),
`resolve_git_mounts` (`:57-98`), `get_nix_volume` (`:149-156`).

New functions:

- `fs_norm_arch <arch>` — `amd64|x64|x86_64` → `x86_64`; `arm64|aarch64` → `aarch64`.
- `fs_host_arch()` — `fs_norm_arch "$(uname -m)"`.
- `fs_in_container()` — returns 0 inside a container. Mirrors `platform.rs:191-218`: `/.dockerenv`,
  `$KUBERNETES_SERVICE_HOST`, `$CONTAINER`/`$container`, `/proc/1/cgroup` matching `docker|kubepods|containerd`.
- `fs_native_blocker <target_arch>` — echoes the human-readable reason native is unavailable, or empty
  if it is available. Checked in order: not Linux → cross-arch (`target != host`) → no `nix` on PATH →
  no `/nix/store` → in a container. Returning a *string* rather than logging is what keeps this file
  free of `log_*`.
- `fs_choose_strategy <target_arch>` — echoes `native` or `docker`. **Reads
  `$FIRESTREAM_BUILD_STRATEGY` (`auto`|`native`|`docker`) first, before any probing**, so forcing
  `docker` is a total rollback with no code revert. Otherwise `native` iff `fs_native_blocker` is empty.
- `fs_nix_build_native <flake_dir> <flake_ref> <dest> [--dir]` —
  `cd "$flake_dir" && nix build "$flake_ref" --out-link "$dest" -L --no-update-lock-file
  --extra-experimental-features "nix-command flakes"`.
  `rm -rf "$dest"` first (see risks). `--extra-experimental-features` removes the need for a separate
  capability probe on hosts where flakes aren't globally on.
- `fs_nix_build_docker <flake_dir> <flake_ref> <dest> <target_arch> [--dir] [--sock]` — the existing
  `docker run` block lifted verbatim from `container-images.sh:185-252`, including
  `--mount type=volume,source=$(get_nix_volume …),target=/nix`, `resolve_git_mounts`, and the inner
  `sh -c`. The `command -v docker` check (`container-images.sh:145-149`) and the `nixos/nix:latest`
  pull (`:151-156`) move **inside** this function — they are unconditional preambles today.
- `fs_build_image <flake_dir> <flake_ref> <dest> <target_arch> [flags]` — dispatches on
  `fs_choose_strategy`, logging the chosen strategy and, when falling back, the blocker string.

**Why `--out-link` (chosen):** `_build/<pkg>/<pkg>.tar.gz` becomes a symlink into the store — zero bytes
copied, and it registers a **GC root** so `nix store gc` can't reap a freshly built image before
`docker load`. Downstream is unchanged because `[[ -s … ]]`, `docker load <`, and `ls` all follow symlinks.
I grepped for other consumers: only `makefile:939` / `:947` (`_build/manifest/*`) and
`src/app/firestream-tui/src/backend/bootstrap.rs:170` (the `.build-batch.lock` staleness warning)
read anything under `_build/`. Nothing reads the per-package tarball outside `container-images.sh`.

### 2. `bin/build/_common.sh`

- `source "$(dirname "${BASH_SOURCE[0]}")/strategy.sh"` right after the `set -euo pipefail` at `:13`.
- Delete the three functions moved to `strategy.sh`.
- **Make the Docker resource probes lazy.** `_detect_docker_cpus` / `_detect_docker_memory` (`:125-145`)
  run `docker info` **at source time**; on the native path that is two pointless round-trips (and a
  multi-second stall plus noise on a host with no daemon). Wrap the `DOCKER_CPUS`/`DOCKER_MEMORY`/`DOCKER_SWAP`
  assignments in `fs_docker_resources()`, called only from `fs_nix_build_docker`.
- Leave `CONTAINER_REGISTRY` (`:160-204`) untouched — it stays the authority for `redis` → `redis-7`.

### 3. `bin/build/container-images.sh`

Keep arg parsing, container validation (`:81-89`), the `resolve_package_name` loop (`:92-97`), the batch
lock (`:104-143`), and the summary block **exactly as they are** — this is what preserves the registry
semantics.

- Add `--native` / `--docker` flags to the parser (`:55-81`), each exporting `FIRESTREAM_BUILD_STRATEGY`.
  Names match the already-shipped `firestream build --native/--docker` (`cli/args.rs:190-211`).
- Delete `:144-201` wholesale — the docker check, builder pull, arch→platform map, `get_nix_volume`,
  `resolve_git_mounts`, and the `docker_args` array all now live in `fs_nix_build_docker`.
- Replace the `docker run … &` block (`:230-252`) with
  `fs_build_image "$REPO_ROOT" ".#$pkg" "$OUT_DIR/${pkg}.tar.gz" "$TARGET_ARCH" --sock > >(tee "$OUT_DIR/build.log") 2>&1 &`
  and keep the surrounding `wait`/`INTERRUPTED`/`cleanup` machinery. **Rename `DOCKER_PID` → `BUILD_PID`**
  (`:253-257`, and the `cleanup` trap at `:113-118`) — it may now be a native `nix` process.
- `docker load < …` at `:271` and the `sed -n 's/.*Loaded image: //p'` tag extraction at `:273` are unchanged.
- Delete the DEPRECATED notice at `:16-18`. This script becomes the shared implementation again;
  pointing users at a CLI that resolves `redis` to a different version is actively harmful.

### 4. `bin/build/manifest.sh`

- Add the same `--native` / `--docker` flags (`:42-56`).
- Delete `:74-126` (docker preamble + `docker_args`).
- Replace the `docker run` at `:131-151` with
  `fs_build_image "$REPO_ROOT" "$NIX_TARGET" "$OUTPUT_DIR" "$TARGET_ARCH" --dir`.
- The `cp -rL /tmp/result /tmp/result-writable; chmod -R u+w` dance (`:145-148`) exists solely to work
  around read-only store paths plus macOS volume permissions; it stays in `fs_nix_build_docker`'s
  `--dir` branch and is unnecessary natively. `_build/manifest` becomes a GC-rooted symlink, so
  `makefile:939` (`_build/manifest/sbom-cyclonedx.json`) still resolves and `makefile:947`'s `rm -rf` still works.

### 5. `nix/flake-modules/docker-build.nix`

- `source ${../../bin/build/strategy.sh}` immediately after the `export PATH=…` at `:24`.
  **Do not add `pkgs.nix` to `makeBinPath`** — that line already ends `:"$PATH"`, and `nix` is not in the
  list, so the host's `nix` (and host daemon socket) resolves correctly. Adding a store `nix` would bloat
  the closure and risk a client/daemon version mismatch.
- Add `--native` / `--docker` to the arg loop (`:59-70`) and to `usage()`.
- Relax the hard `command -v docker` failure at `:73` — docker is now required only for the docker
  strategy or for `--load`.
- Replace the arch→platform+volume `case` (`:76-80`) with `get_nix_volume` from the shared library,
  moved inside the docker branch.
- Keep the `FLAKE_DIR` resolution (`:82-103`) and the `/nix/store` vs live-worktree `case` (`:105-140`)
  as-is — that logic is genuinely specific to this app. It already computes exactly the
  `(WORKDIR, FLAKE_REF)` pair `fs_build_image` needs. On the **native** branch pass `WORKDIR` plus
  `".#$PKG"` uniformly and skip the `/flake#` form: a store snapshot is itself a valid flake directory,
  so the whole bind-mount/worktree apparatus is bypassed.
- Replace the `docker run` at `:143-152` with
  `fs_build_image "$WORKDIR" "$FLAKE_REF" "$OUT_DIR/$PKG.tar.gz" "$TARGET_ARCH"`.
  The tarball check (`:154-158`) and `docker load` (`:159-165`) are unchanged.

`mkImageApp` (`:165-170`), `_module.args.firestreamBuildImage` (`:173`), and the `compose.nix:231`
consumer need **no changes** — `apps.<name>-image` and `apps.<name>-up` pick up native builds automatically.

### 6. `makefile`

- Near `ARCH ?=` (`:873`), add an exported passthrough so every existing target inherits it with no
  further edits:
  ```make
  # Build strategy: auto (default) | native | docker
  STRATEGY ?=
  ifneq ($(STRATEGY),)
  export FIRESTREAM_BUILD_STRATEGY := $(STRATEGY)
  endif
  ```
  This covers `container-build-%` (`:113`), the six per-app `*-build` targets (`:144,200,244,306,452,542`)
  via `BUILD_CONTAINER` (`:109`), `manifest`/`sbom-%` (`:927-933`), and `flake-image-%` (`:875`).
- **Do not touch `BUILD_CONTAINER` at `:109`.**
- `builder-cache-stats` (`:900-907`): relabel the volume listing "Docker fallback cache (macOS / cross-arch only)"
  and add a host-store line guarded on `command -v nix`.
- `builder-cache-clean` (`:909-913`): behavior unchanged. Update the echo to note this is now what
  reclaims the 118 GB, and that `firestream-nix-store-arm64` is still needed for `ARCH=aarch64`.
- Fix the stale header comment at `:856-857` ("Docker-based, works on macOS").

---

## Phase 2 — Rust builder fixes (separate commits)

All four verified by reading the code; none change any consumer's interface.

1. **`strategy/native.rs:36-40`** — add `-L` and `--no-update-lock-file`, and switch from `.output()`
   (which buffers everything until exit) to `.stdout(piped()).stderr(inherit()).spawn()` so progress
   reaches the terminal while `--print-out-paths` is still captured. Today a 30-minute Spark build prints
   nothing. Highest-value fix; the Docker path already passes `-L` (`docker.rs:313+`).
2. **`builder.rs:265`** — `build_container_with_progress` unconditionally
   `std::fs::remove_file(&tarball_path)` after loading. On the native path that path is in `/nix/store`,
   which is read-only. Warn-only today, a hard error the moment anyone promotes that `warn!`.
   Guard with `if !tarball_path.starts_with("/nix/store")`. (`build_package_with_progress` at `:483`
   already omits this — good.)
3. **`main.rs:228`** — `.with_force_docker(docker || !native)` makes `--native` the only way to ever get
   a native build, even with a real `--containers-dir`. Change to `.with_force_docker(docker)` and let
   `BuildConfig::for_embedded()`'s own `force_docker: true` (`config.rs:97-110`) carry the genuinely
   embedded case. Matches `firestream build`'s semantics.
4. **`config.rs:44-67`** — `default_package_registry` is a third copy of data the flake already owns, and
   it has already drifted from `_common.sh:160-204` (missing `airflow:3`, `kafka:4`, `spark:4`,
   `jupyterhub:5`, `odoo:15..18`). It is also dead code on the `firestream build` path — `commands.rs:828-845`
   never calls `resolve_package_name`. Either delete it or fix it to match and add a parity test.
   Low urgency; do it last.

---

## Commit order and rollback

Each commit leaves the tree working and is independently revertable:

1. `strategy.sh` + `_common.sh` sourcing + lazy docker probes — **no behavior change** (nothing calls
   `fs_choose_strategy` yet).
2. `container-images.sh` native branch. Verify.
3. `manifest.sh` native branch. Verify.
4. `docker-build.nix` native branch. Verify.
5. `makefile` `STRATEGY` var + comment/cache-target updates.
6-9. The four Rust fixes.

**Rollback:** `export FIRESTREAM_BUILD_STRATEGY=docker` restores byte-identical prior behavior across all
three shell/Nix paths with no code revert. That is why the env var is read at the very top of the predicate,
before any probing.

## Risks

- **Stale root-owned `_build/` artifacts.** Past Docker builds wrote `_build/<pkg>/<pkg>.tar.gz` and
  `_build/manifest/` as root. `nix build --out-link` refuses to clobber a non-symlink, and a non-root
  user may not be able to remove them. Mitigated by the `rm -rf "$dest"` inside `fs_nix_build_native`,
  plus a one-time `sudo rm -rf _build` in the migration note.
- **`_build` entries become symlinks.** Verified no other consumer; `-s`, `<`, `docker load -i`, `tar -tf`,
  and `ls -L` all follow them.
- **New GC roots under `_build/`.** Built images now pin store paths on the host. Intentional — this is
  the cache that replaces the volume — but `nix store gc` won't reclaim until `_build` is cleared.
  Document in `builder-cache-clean`.
- **Host `/nix/store` growth.** The tonnage previously in the Docker volume now lands on `/`. That is the
  intended tradeoff; the host store is shared with everything else on the machine, so the marginal cost
  is far below 118 GB.
- **Untrusted users.** On a host where `$USER` is not in `trusted-users`, `nix build` silently ignores
  flake-supplied `extra-substituters` and rebuilds from source. Add a one-line warning in
  `fs_nix_build_native`. (Not an issue here — `justin` is trusted.)
- **Nested nix.** `docker-build.nix`'s app runs under `nix run`, which may export `NIX_*` vars that the
  inner `nix build` inherits. Low risk, but it is the most likely surprise — covered by verification V5.
- **Devcontainer.** `fs_in_container` returns true there, so it keeps the Docker path unchanged,
  including the `/var/run/docker.sock` mount.

## The 118 GB volume

The code change deletes nothing. Once V1-V6 pass, reclaim it deliberately with
`docker volume rm firestream-nix-store-amd64` (or `make builder-cache-clean`, which also drops arm64).
Keep `firestream-nix-store-arm64` if you build `ARCH=aarch64`; on a macOS host both stay live.

---

## Verification

```bash
# from the repo root

# V1 — decision predicate, no builds
bash -c 'source bin/build/strategy.sh
  echo "auto:   $(fs_choose_strategy x86_64)"                                  # native
  echo "cross:  $(fs_choose_strategy aarch64)"                                 # docker
  echo "why:    [$(fs_native_blocker aarch64)]"                                # cross-arch target...
  FIRESTREAM_BUILD_STRATEGY=docker; export FIRESTREAM_BUILD_STRATEGY
  echo "forced: $(fs_choose_strategy x86_64)"'                                 # docker
docker run --rm -v "$PWD:$PWD:ro" -w "$PWD" nixos/nix:latest \
  bash -c 'source bin/build/strategy.sh; fs_choose_strategy x86_64'            # docker (/.dockerenv)

# V2 — native build spawns no builder container
sudo rm -rf _build                       # clear root-owned artifacts from past Docker builds
docker ps -a --format '{{.Image}}' | sort > /tmp/before.txt
time ./bin/build-container.sh redis
docker ps -a --format '{{.Image}}' | sort > /tmp/after.txt
diff /tmp/before.txt /tmp/after.txt      # expect: no new nixos/nix container
ls -l _build/redis-7/redis-7.tar.gz      # symlink -> /nix/store/...
nix-store --query --roots "$(readlink -f _build/redis-7/redis-7.tar.gz)"   # GC root present

# V3 — registry semantics unchanged (the redis landmine)
bash -c 'source bin/build/_common.sh; resolve_package_name redis ""'          # redis-7
bash -c 'source bin/build/_common.sh; resolve_package_name postgresql 16'     # postgresql-16
docker image inspect firestream-redis:7-nix >/dev/null && echo "redis-7 OK"

# V4 — digest equivalence, native vs docker
P=$(nix build .#redis-7 --no-link --print-out-paths); sha256sum "$P"
FIRESTREAM_BUILD_STRATEGY=docker ./bin/build-container.sh redis
sha256sum "$(readlink -f _build/redis-7/redis-7.tar.gz)"   # must match $P's sum
# Cheaper structural check — same derivation implies byte-identical image:
nix eval --raw .#packages.x86_64-linux.redis-7.drvPath

# V5 — docker-build.nix, all four branches incl. the store-snapshot native path
nix run .#postgresql-image -- --load                        # native, live worktree
nix run .#postgresql-image -- --load --docker               # forced docker
nix run .#postgresql-image -- --target aarch64 --no-load    # cross-arch -> docker
(cd /tmp && nix run "$PWD_REPO#postgresql-image" -- --no-load)  # /nix/store FLAKE_DIR, native
nix run .#postgresql-up                                     # compose.nix:231 path

# V6 — manifest / SBOM both strategies
make manifest && ls -lL _build/manifest/ && make manifest-validate
make sbom-airflow && ls -lL _build/sbom-airflow/
FIRESTREAM_BUILD_STRATEGY=docker make manifest
make manifest-clean && ls _build/                           # no permission errors

# V7 — escape hatches through make
make redis-build STRATEGY=docker 2>&1 | grep -i 'nix store volume'
make redis-build STRATEGY=native 2>&1 | grep -i 'nativ'

# V8 — Rust fixes
cargo test -p nix-container-builder
cargo run -p firestream -- build redis-7 2>&1 | head -5     # streamed nix output now visible
```

**What a good run looks like:** V2 shows zero new `nixos/nix` containers and a wall-clock drop; V4's two sha256 sums
match (they must — same derivation; a mismatch means the Docker path is building a different flake revision,
which is itself a bug worth finding); V3 confirms `redis` still means redis-7; V5's four invocations cover
every branch of `docker-build.nix`; V6 leaves `_build/manifest/sbom-cyclonedx.json` readable.

---

## Files

**New:** `bin/build/strategy.sh`

**Modified:** `bin/build/_common.sh`, `bin/build/container-images.sh`, `bin/build/manifest.sh`,
`nix/flake-modules/docker-build.nix`, `makefile`,
`src/lib/rust/nix-container-builder/src/{strategy/native.rs,builder.rs,main.rs,config.rs}`
