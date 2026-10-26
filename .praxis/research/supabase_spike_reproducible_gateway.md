---
id: FIRE-040
title: "Spike: reproducible Supabase gateway (Envoy deps or nginx variant)"
type: research
status: done
priority: high
tags: [envoy, spike, supabase]
affects: []
depends-on: []
---

# Spike: reproducible Supabase gateway (Envoy deps or nginx variant)

Epic: [[FIRE-009]]. This spike blocks [[FIRE-039]].

## Problem

The Envoy image (`supabase-envoy`) cannot be rebuilt reliably.
`src/containers/firestream/supabase-envoy/module.nix:40-42` overrides `pkgs.envoy` `depsHash`, because the nixpkgs hash was already stale. On 2026-10-23 the Envoy binary was gone from the store, and the rebuild of `envoy-1.36.2-deps.tar.gz` failed with a fixed-output hash mismatch. The Bazel dependency fetch gives different bytes over time. CI and every other machine therefore cannot build the image.

## Questions

1. **The cause:** which inputs of the deps fetch change between runs? Look for an unpinned Bazel repository, a moving URL, timestamps, or order effects. Compare two fetches.
2. **Can the fetch be made stable?** Try pinning or excluding the moving inputs, normalizing the tarball, or an Envoy version whose nixpkgs `depsHash` is current. Two clean fetches must give the same hash.
3. **The nginx variant:** what does upstream `docker-compose.nginx.yml` (self-hosted v0.8.3) do? Does it translate `sb_publishable_*` and `sb_secret_*` keys, check the anon and service keys, route Realtime websockets with the tenant Host header, and serve Studio with basic auth? Can it run on Firestream's nginx image (`src/containers/firestream/nginx/`)?
4. **Other options:** host the known-good deps tarball as a pinned artifact, or use a binary cache. Give their cost and their risk.

## Exit

Write the result in the ticket log: each option with evidence, and a recommendation. Do not change the Envoy build or the gateway in this spike. Scratch work goes in `/tmp/fire-gateway-spike/`.

## Result

**Verdict:** replace the monolithic deps FOD with a build that pins each dependency on its own. Keep Envoy as the gateway.

**Evidence:**
- The two stored deps tarballs (`qgjaz6v8…` and `lgam82yg…`) are byte-identical: `cmp` exits 0, and a `tar -tvzf` diff over 111,990 entries is empty. This round did not drift. The `.lock` file is left over from a killed build.
- nixpkgs' own `depsHash` (`sha256-cUpCkmJm…`, x86_64-linux) differs from the Firestream override (`sha256-J1TL/F9n…`). So the hash already changed between fetch rounds.
- The likely source of drift: nixpkgs sets `fetchAttrs.env.CARGO_BAZEL_REPIN = true`, which resolves the crate graph again against the live crates.io index at fetch time. nixpkgs' Envoy history shows other drift sources: the system Go, a coursier binary, the Go SDK listing JSON. Each fix was a patch over one big snapshot.
- The nginx variant is not an alternative gateway. Upstream `docker-compose.nginx.yml` adds an nginx-certbot front door that forwards every request to Envoy. Only Envoy's Lua filters do the key checks, the `sb_` key translation and the Realtime Host rewrite. Firestream's nginx image has no Lua.

**Options:**
- A hosted deps tarball conflicts with the user's rule against binaries and opaque artifacts.
- A binary cache speeds up builds, but it is not a pin.
- A GC root only helps this machine.

**Follow-up:** the "Pinned source builds for Supabase" epic. Envoy moves to 1.39.1, with each dependency fetched on its own and Bazel run offline.
