"""Find machine code in Supabase app outputs that no allowlist entry covers.

Usage: scan-binaries.py CONFIG.json

CONFIG is a JSON list of targets:

    [{"name": "storage", "root": "/nix/store/...-storage-1.81.0",
      "drv": "storage-1.81.0", "allowAll": null,
      "allow": [{"pattern": "regex", "builtBy": "...", "reason": "..."}]}]

A file is machine code when its first bytes are an ELF, Mach-O, PE or
WebAssembly header. Symlinks are resolved and each real file is read once.
A file passes when `allowAll` is set or an `allow` pattern fully matches its
path relative to `root`. An `allow` entry that matches no file is stale and
fails the scan too, so the allowlist cannot outlive the files it excuses.

Exit status: 0 when every binary is allowed and no entry is stale, 1
otherwise. The report is a table on stdout.
"""

import json
import os
import re
import struct
import sys

MACHO = {
    b"\xfe\xed\xfa\xce", b"\xce\xfa\xed\xfe",
    b"\xfe\xed\xfa\xcf", b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe", b"\xbe\xba\xfe\xca",
}


def kind(path):
    try:
        with open(path, "rb") as f:
            head = f.read(64)
            if head[:4] == b"\x7fELF":
                return "ELF"
            if head[:4] == b"\0asm":
                return "WASM"
            if head[:4] in MACHO:
                return "Mach-O"
            if head[:2] == b"MZ" and len(head) >= 64:
                (pe_offset,) = struct.unpack_from("<I", head, 0x3C)
                f.seek(pe_offset)
                if f.read(4) == b"PE\0\0":
                    return "PE"
    except OSError:
        return None
    return None


def walk(root):
    """Yield (logical path, real path) for every file under root."""
    seen_dirs = set()
    stack = [root]
    while stack:
        top = stack.pop()
        real_top = os.path.realpath(top)
        if real_top in seen_dirs:
            continue
        seen_dirs.add(real_top)
        for entry in os.scandir(top):
            real = os.path.realpath(entry.path)
            if os.path.isdir(real):
                stack.append(entry.path)
            elif os.path.isfile(real):
                yield entry.path, real


def relative(root, path):
    return os.path.relpath(path, root) if path.startswith(root + "/") else path


def scan(target):
    root = target["root"]
    files = {}
    for logical, real in walk(root):
        if real not in files or logical < files[real]:
            files[real] = logical
    found = []
    for real, logical in files.items():
        k = kind(real)
        if k is not None:
            found.append((relative(root, real), k))
    return sorted(found)


def main():
    with open(sys.argv[1]) as f:
        targets = json.load(f)

    rows, allowed, stale = [], 0, []
    for t in targets:
        patterns = [(re.compile(a["pattern"]), a) for a in t["allow"]]
        used = set()
        for path, k in scan(t):
            if t["allowAll"]:
                allowed += 1
                continue
            hit = next((a for rx, a in patterns if rx.fullmatch(path)), None)
            if hit is None:
                rows.append((t["name"], t["drv"], k, path))
            else:
                allowed += 1
                used.add(hit["pattern"])
        stale += [(t["name"], a["pattern"]) for a in t["allow"] if a["pattern"] not in used]

    print(f"scanned {len(targets)} app outputs; {allowed} binaries allowed")
    if rows:
        widths = [max(len(r[i]) for r in rows + [("image", "derivation", "kind", "path")])
                  for i in range(4)]
        line = "  ".join("{:<%d}" % w for w in widths)
        print(f"\n{len(rows)} binaries with no allowlist entry:\n")
        print(line.format("image", "derivation", "kind", "path"))
        print(line.format(*("-" * w for w in widths)))
        for r in rows:
            print(line.format(*r))
    if stale:
        print(f"\n{len(stale)} allowlist entries match no file:\n")
        for name, pattern in stale:
            print(f"  {name}: {pattern}")
    return 1 if rows or stale else 0


if __name__ == "__main__":
    sys.exit(main())
