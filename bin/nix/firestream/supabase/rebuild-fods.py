#!/usr/bin/env python3
"""Fetch each fixed-output derivation of the Supabase images again.

Usage: rebuild-fods.py [options] INSTALLABLE...

The script reads the derivation closure of each INSTALLABLE with
`nix derivation show --recursive`. It keeps each fixed-output derivation
(FOD): a derivation whose output declares a content hash. Then it removes
each FOD whose output the trusted cache serves. The pinning policy
(docs/nix-pinning-policy.md) trusts nixpkgs and cache.nixos.org, so those
fetches are not Firestream pins.

For each remaining FOD it runs `nix build`, which makes sure the output is
valid, and then `nix build --rebuild`, which fetches the source again and
compares the result with the declared hash. A FOD reproduces when the
rebuild succeeds. A hash mismatch is drift. A failed fetch is drift too:
the pinned source is gone.

The enumeration reads .drv files only. It does no import-from-derivation.

Exit status: 0 when every selected FOD reproduces, 1 otherwise.
"""

import argparse
import concurrent.futures
import json
import re
import subprocess
import sys
import urllib.error
import urllib.request

FOD_ERROR = re.compile(
    r"(hash mismatch in fixed-output derivation|"
    r"Cannot build|builder for|may not be deterministic)[^']*'(/nix/store/[^']+\.drv)'"
)


def closure(installables):
    proc = subprocess.run(
        ["nix", "derivation", "show", "--recursive", *installables],
        check=True, stdout=subprocess.PIPE,
    )
    data = json.loads(proc.stdout)
    return data.get("derivations", data)


def store_path(p):
    return p if p.startswith("/") else "/nix/store/" + p


def fods(drvs):
    found = {}
    for name, drv in drvs.items():
        for out, spec in drv.get("outputs", {}).items():
            if "hash" in spec:
                found[store_path(name)] = store_path(drv["env"][out])
    return found


def served(cache, path):
    digest = path.removeprefix("/nix/store/").split("-", 1)[0]
    req = urllib.request.Request(f"{cache}/{digest}.narinfo", method="HEAD")
    for _ in range(3):
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                return resp.status == 200
        except urllib.error.HTTPError as err:
            if err.code == 404:
                return False
        except (urllib.error.URLError, TimeoutError):
            pass
    raise RuntimeError(f"cannot query {cache} for {path}")


def untrusted(found, cache, workers):
    with concurrent.futures.ThreadPoolExecutor(workers) as pool:
        hits = dict(zip(found, pool.map(lambda d: served(cache, found[d]), found)))
    return {d: o for d, o in found.items() if not hits[d]}


def nix_build(drvs, extra):
    """Build DRVS. Return the set of drvs that nix names in an error."""
    if not drvs:
        return set()
    proc = subprocess.Popen(
        ["nix", "build", "--no-link", "--keep-going", *extra,
         *[d + "^*" for d in drvs]],
        stderr=subprocess.PIPE, text=True,
    )
    failed = set()
    for line in proc.stderr:
        sys.stderr.write(line)
        failed |= {m.group(2) for m in FOD_ERROR.finditer(line)}
    selected = set(drvs)
    named = failed & selected
    if proc.wait() != 0 and not named:
        # NOTE: an error that names no selected drv (an evaluation or daemon
        # fault) must not pass silently, so the whole batch counts as failed.
        return selected
    return named


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("installables", nargs="+")
    ap.add_argument("--cache", default="https://cache.nixos.org",
                    help="trusted binary cache; '' keeps every FOD")
    ap.add_argument("--match", help="keep only FODs whose drv path matches REGEX")
    ap.add_argument("--limit", type=int, help="keep every k-th FOD, at most N")
    ap.add_argument("--batch", type=int, default=200)
    ap.add_argument("--workers", type=int, default=32)
    ap.add_argument("--list", action="store_true", help="print the FODs and stop")
    ap.add_argument("--report", help="write a JSON report to this file")
    args = ap.parse_args()

    found = fods(closure(args.installables))
    total = len(found)
    if args.cache:
        found = untrusted(found, args.cache.rstrip("/"), args.workers)
    selected = sorted(found)
    if args.match:
        selected = [d for d in selected if re.search(args.match, d)]
    if args.limit and len(selected) > args.limit:
        step = len(selected) / args.limit
        selected = [selected[int(i * step)] for i in range(args.limit)]

    print(f"fods: {total} in closure, {len(found)} not in trusted cache, "
          f"{len(selected)} selected", flush=True)
    if args.list:
        print("\n".join(selected))
        return 0

    failed = set()
    batches = [selected[i:i + args.batch] for i in range(0, len(selected), args.batch)]
    for n, batch in enumerate(batches, 1):
        print(f"batch {n}/{len(batches)}: realise", flush=True)
        failed |= nix_build(batch, [])
        todo = [d for d in batch if d not in failed]
        print(f"batch {n}/{len(batches)}: rebuild {len(todo)}", flush=True)
        failed |= nix_build(todo, ["--rebuild"])

    report = {"closure": total, "untrusted": len(found),
              "selected": len(selected), "failed": sorted(failed)}
    if args.report:
        with open(args.report, "w") as fh:
            json.dump(report, fh, indent=2)
    for drv in sorted(failed):
        print(f"DRIFT {drv}")
    print(f"reproduced {len(selected) - len(failed)}/{len(selected)}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
