#!/usr/bin/env python3
"""Write deps.json: one pinned download for each archive that the offline
Envoy build of //source/exe:envoy-static needs.

Usage:
  generate-deps.py ENVOY_SRC [-o deps.json] [--repository-cache DIR]
                   [--previous deps.json]

ENVOY_SRC is the unpatched Envoy source tree. Every URL and sha256 comes from
an upstream file:

  - bazel/repository_locations.bzl and api/bazel/repository_locations.bzl;
  - Cargo.Bazel.lock (the crates of the Rust SDK and its builtin extensions);
  - the .bzl file that declares an archive inside another pinned archive
    (rules_rust, cel-cpp, emsdk, ...);
  - the go_repository declarations, for the Go modules of protoc-gen-validate;
  - the Rust release channel manifest of the version that
    bazel/dependency_imports.bzl names, for the fenix toolchain.

The script reads the declaring archives from --repository-cache (a Bazel
content_addressable tree, as the Nix check passes it) or downloads them.

A Go module has a go.sum `h1:` hash, not a file hash. The script downloads the
module .zip and .mod from proxy.golang.org and records their sha256. The Rust
channel manifest has no upstream hash in the Envoy tree; the script keeps the
component URLs and hashes of the manifest that it downloads.

With --previous, the script takes the Go file hashes and the Rust components
from an existing deps.json when their inputs are unchanged. The Nix check uses
it to run offline.
"""

import argparse
import hashlib
import io
import json
import os
import re
import sys
import tarfile
import tempfile
import urllib.request
import zipfile

# NOTE: the entries that an offline `bazel build --nobuild` of envoy-static
# fetches. A missing entry fails that build with "download is disabled".
LOCATIONS = [
    "abseil_cpp", "aspect_bazel_lib", "bazel_features", "bazel_gazelle",
    "bazel_skylib",
    "boringssl", "brotli", "build_bazel_rules_apple", "c_ares", "cel_cpp",
    "com_envoyproxy_protoc_gen_validate",
    "com_github_chrusty_protoc_gen_jsonschema", "com_github_grpc_grpc",
    "com_google_googleapis", "com_google_protobuf", "cpp2sky", "dd_trace_cpp",
    "dev_cel", "emsdk", "envoy_toolshed", "flatbuffers", "fmt", "googleurl",
    "grpc_httpjson_transcoding", "hessian2_codec", "io_bazel_rules_go",
    "libcircllhist", "libevent", "libmaxminddb", "liburing", "luajit",
    "msgpack_cxx", "nghttp2", "nlohmann_json", "ocp", "opentelemetry_cpp",
    "opentelemetry_proto", "platforms", "prometheus_metrics_model",
    "proto_converter", "proto_field_extraction", "proto_processing",
    "proxy_wasm_cpp_host", "proxy_wasm_cpp_sdk", "proxy_wasm_rust_sdk",
    "quiche", "re2", "rules_buf", "rules_cc", "rules_foreign_cc",
    "rules_fuzzing", "rules_java", "rules_license", "rules_pkg", "rules_proto",
    "rules_proto_grpc", "rules_python", "rules_rust", "rules_shell",
    "shellcheck", "skywalking_data_collect_protocol", "spdlog", "tclap",
    "tcmalloc", "toolchains_llvm", "v8", "wuffs", "xds", "xxhash", "yaml_cpp",
    "yq_bzl", "zipkin_api", "zlib_ng", "zstd",
]

# NOTE: prebuilt binaries that the upstream build downloads, and what
# replaces each one. The script fails if the needed set holds one of them.
EXCLUDED = {
    "com_google_protobuf_protoc_*": "prebuilt protoc; Bazel builds protoc from the pinned protobuf source",
    "fips_*": "prebuilt Go, CMake and Clang for the FIPS build; not built",
    "llvm_toolchain_llvm": "prebuilt LLVM 18; nixpkgs llvm and clang (BAZEL_LLVM_PATH)",
    "sysroot_linux_*": "prebuilt Debian sysroot; the nixpkgs glibc headers",
    "libcxx_libs_*": "prebuilt libc++; the nixpkgs GCC libstdc++",
    "python3_12_*": "prebuilt CPython 3.12.3; nixpkgs python312",
    "go_sdk / go_*": "prebuilt Go 1.24.6; nixpkgs go (go_version = host)",
    "rust_*__stable_tools": "prebuilt Rust 1.88.0; the fenix 1.88.0 toolchain",
    "cargo_bazel_*": "prebuilt cargo-bazel; built from the rules_rust source",
    "remotejdk*, remote_java_tools*": "prebuilt JDK and Java tools; no Java rule remains",
    "antlr4_jar": "prebuilt ANTLR jar; nixpkgs antlr4_13",
    "rules_buf_toolchains": "prebuilt buf; not in the envoy-static graph",
    "emscripten_bin_*, nodejs_*": "prebuilt emsdk and Node.js; not in the envoy-static graph",
}

# NOTE: archives that a .bzl file inside another pinned archive declares,
# as (declaring location, file in that archive, names). repository_locations
# does not list them.
TRANSITIVE = [
    ("cel_cpp", "bazel/deps.bzl", ["antlr4_runtimes"]),
    ("emsdk", "deps.bzl", ["aspect_rules_js", "rules_nodejs", "bazel_features"]),
    ("build_bazel_rules_apple", "apple/repositories.bzl",
     ["build_bazel_apple_support", "build_bazel_rules_swift"]),
    ("toolchains_llvm", "toolchain/deps.bzl", ["helly25_bzl"]),
    ("aspect_bazel_lib", "lib/repositories.bzl", ["jq.bzl", "tar.bzl"]),
    ("bazel_gazelle", "deps.bzl", ["package_metadata"]),
    ("rules_rust", "rust/private/repository_utils.bzl", ["rules_rust_tinyjson"]),
    ("rules_rust", "cargo/3rdparty/crates/defs.bzl", ["rrc__*"]),
]
# NOTE: crates of the rules_rust cargo tools that no envoy-static action uses.
RRC_UNUSED = {"rrc__cargo_toml-0.20.5", "rrc__memchr-2.7.4"}

# NOTE: the Go modules of protoc-gen-validate. Both declarers name some of
# them; the later declaration in WORKSPACE order wins, as in Bazel.
GO_MODULES = [
    "com_github_golang_protobuf", "com_github_iancoleman_strcase",
    "com_github_lyft_protoc_gen_star_v2", "com_github_spf13_afero",
    "org_golang_google_protobuf", "org_golang_x_mod", "org_golang_x_sys",
    "org_golang_x_text", "org_golang_x_tools",
]
GO_DECLARERS = [
    ("com_github_chrusty_protoc_gen_jsonschema", "deps.bzl"),
    (None, "bazel/dependency_imports.bzl"),
]

CRATE_TRIPLES = {"x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"}
RUST_COMPONENTS = ["cargo", "rust-std", "rustc"]


def fail(msg):
    sys.exit("generate-deps: " + msg)


def load_locations(src):
    spec = {}
    for rel in ("api/bazel/repository_locations.bzl", "bazel/repository_locations.bzl"):
        scope = {}
        exec(compile(open(os.path.join(src, rel)).read(), rel, "exec"), scope)
        spec.update(scope["REPOSITORY_LOCATIONS_SPEC"])

    def fmt(s, v):
        return s.format(version=v, dash_version=v.replace(".", "-"),
                        underscore_version=v.replace(".", "_"))

    out = {}
    for key, loc in spec.items():
        loc = dict(loc)
        if "version" in loc:
            loc["urls"] = [fmt(u, loc["version"]) for u in loc["urls"]]
            if "strip_prefix" in loc:
                loc["strip_prefix"] = fmt(loc["strip_prefix"], loc["version"])
        out[key] = loc
    return out


class Archives:
    def __init__(self, cache, scratch):
        self.cache = cache
        self.scratch = scratch

    def path(self, sha256, urls):
        if self.cache:
            p = os.path.join(self.cache, "content_addressable", "sha256", sha256, "file")
            if os.path.exists(p):
                return p
            fail("%s is not in the repository cache" % sha256)
        p = os.path.join(self.scratch, sha256)
        if not os.path.exists(p):
            data = fetch(urls)
            if hashlib.sha256(data).hexdigest() != sha256:
                fail("sha256 mismatch for %s" % urls[0])
            open(p, "wb").write(data)
        return p

    def read(self, loc, member):
        p = self.path(loc["sha256"], loc["urls"])
        prefix = loc.get("strip_prefix", "")
        want = (prefix + "/" + member) if prefix else member
        data = open(p, "rb").read()
        if data[:2] == b"PK":
            with zipfile.ZipFile(io.BytesIO(data)) as z:
                return z.read(want).decode()
        with tarfile.open(fileobj=io.BytesIO(data)) as t:
            for m in t.getmembers():
                if m.name.lstrip("./") == want:
                    return t.extractfile(m).read().decode()
        fail("%s not found in %s" % (want, loc["urls"][0]))


def fetch(urls):
    for url in urls:
        try:
            with urllib.request.urlopen(url, timeout=120) as r:
                return r.read()
        except OSError as e:
            print("generate-deps: %s: %s" % (url, e), file=sys.stderr)
    fail("cannot download %s" % urls[0])


def string_constants(text):
    return {m.group(1): m.group(2)
            for m in re.finditer(r'^\s*([A-Z][A-Z0-9_]*)\s*=\s*"([^"]*)"\s*$', text, re.M)}


def call_block(text, name):
    """The rule call that holds `name = "<name>"`, and the string constants
    of the file and of the enclosing function."""
    m = re.search(r'^\s*name = "%s",\n' % re.escape(name), text, re.M)
    if not m:
        fail("no declaration of %s" % name)
    start = text.rfind("(", 0, m.start())
    depth, i = 0, start
    while i < len(text):
        if text.startswith('"""', i):
            i = text.index('"""', i + 3) + 3
            continue
        c = text[i]
        if c == '"':
            i = text.index('"', i + 1) + 1
            continue
        depth += {"(": 1, ")": -1}.get(c, 0)
        i += 1
        if depth == 0:
            break
    fn = text.rfind("\ndef ", 0, start)
    consts = string_constants("\n".join(
        l for l in text.splitlines() if not l.startswith((" ", "\t"))))
    consts.update(string_constants(text[fn if fn >= 0 else 0:start]))
    body = re.sub(r'"""(.|\n)*?"""', '""', text[start + 1:i - 1])
    return body, consts


def block_attrs(block, consts):
    def ev(expr):
        return eval(expr, {"__builtins__": {}}, dict(consts))
    sha = re.search(r'^\s*sha256 = ("[0-9a-f]{64}"),', block, re.M)
    urls = re.search(r"^\s*urls = (\[.*?\]),", block, re.M | re.S)
    url = re.search(r"^\s*url = (.+?),\n", block, re.M)
    if not sha:
        fail("no sha256 in:\n" + block)
    return {"sha256": ev(sha.group(1)),
            "urls": ev(urls.group(1)) if urls else [ev(url.group(1))]}


def crate_closure(lock):
    crates, conds = lock["crates"], lock["conditions"]

    def items(sel):
        out = list(sel.get("common", []))
        for cond, lst in sel.get("selects", {}).items():
            if set(conds.get(cond, [])) & CRATE_TRIPLES:
                out += lst
        return out

    roots = [m for m, path in lock["workspace_members"].items()
             if not path.startswith("test/")]
    seen, stack = set(), list(roots)
    while stack:
        cid = stack.pop()
        if cid in seen:
            continue
        seen.add(cid)
        for attrs in ("common_attrs", "build_script_attrs"):
            a = crates[cid].get(attrs) or {}
            for kind in ("deps", "proc_macro_deps"):
                stack += [i["id"] for i in items(a.get(kind, {}))]
    out = []
    for cid in sorted(seen):
        http = (crates[cid].get("repository") or {}).get("Http")
        if http:
            out.append({"name": "envoy_rust_crate_index__" + cid.replace(" ", "-"),
                        "urls": [http["url"]], "sha256": http["sha256"],
                        "source": "Cargo.Bazel.lock"})
    return out


def go_escape(path):
    return re.sub(r"[A-Z]", lambda m: "!" + m.group(0).lower(), path)


def go_modules(src, locations, archives, known):
    decls = {}
    for loc_key, rel in GO_DECLARERS:
        text = (archives.read(locations[loc_key], rel) if loc_key
                else open(os.path.join(src, rel)).read())
        for name in GO_MODULES:
            if re.search(r'name = "%s",' % name, text):
                b, _ = call_block(text, name)
                get = lambda a: re.search(r'%s = "([^"]+)"' % a, b).group(1)
                decls[name] = {"importpath": get("importpath"),
                               "version": get("version"), "sum": get("sum")}
    out = []
    for name in GO_MODULES:
        if name not in decls:
            fail("no go_repository for %s" % name)
        d = decls[name]
        base = "https://proxy.golang.org/%s/@v/%s" % (go_escape(d["importpath"]), d["version"])
        entry = dict(name=name, **d)
        prev = known.get(name)
        for ext in ("zip", "mod"):
            if prev and all(prev[k] == d[k] for k in d):
                sha = prev[ext]["sha256"]
            else:
                sha = hashlib.sha256(fetch([base + "." + ext])).hexdigest()
            entry[ext] = {"url": base + "." + ext, "sha256": sha}
        out.append(entry)
    return out


def rust_channel(src, previous):
    text = open(os.path.join(src, "bazel/dependency_imports.bzl")).read()
    m = re.search(r"rust_register_toolchains\((.|\n)*?versions = \[\"([0-9.]+)\"\]", text)
    if not m:
        fail("no Rust version in bazel/dependency_imports.bzl")
    version = m.group(2)
    if previous and previous.get("version") == version:
        return previous
    import tomllib
    url = "https://static.rust-lang.org/dist/channel-rust-%s.toml" % version
    manifest = tomllib.loads(fetch([url]).decode())
    pkg = {}
    for name in RUST_COMPONENTS:
        pkg[name] = {"target": {}}
        for triple in sorted(CRATE_TRIPLES):
            t = manifest["pkg"][name]["target"][triple]
            if not t.get("available"):
                fail("Rust %s has no %s for %s" % (version, name, triple))
            pkg[name]["target"][triple] = {
                "available": True, "url": t["xz_url"], "hash": t["xz_hash"]}
    return {"version": version, "date": manifest["date"], "pkg": pkg}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("src")
    ap.add_argument("-o", "--output", default="-")
    ap.add_argument("--repository-cache")
    ap.add_argument("--previous")
    args = ap.parse_args()

    locations = load_locations(args.src)
    scratch = tempfile.mkdtemp(prefix="envoy-deps-")
    archives = Archives(args.repository_cache, scratch)

    entries = []
    for key in LOCATIONS:
        loc = locations[key]
        entries.append({"name": key, "urls": loc["urls"], "sha256": loc["sha256"],
                        "source": "repository_locations.bzl"})

    for loc_key, rel, names in TRANSITIVE:
        text = archives.read(locations[loc_key], rel)
        if names == ["rrc__*"]:
            names = sorted(set(re.findall(r'name = "(rrc__[^"]+)"', text)) - RRC_UNUSED)
        for name in names:
            attrs = block_attrs(*call_block(text, name))
            entries.append(dict(name=name, source="%s/%s" % (loc_key, rel), **attrs))

    lock = json.load(open(os.path.join(args.src, "Cargo.Bazel.lock")))
    entries += crate_closure(lock)

    previous = json.load(open(args.previous)) if args.previous else {}
    known = {m["name"]: m for m in previous.get("goModules", [])}
    gomods = go_modules(args.src, locations, archives, known)
    rust = rust_channel(args.src, previous.get("rust"))

    by_sha = {}
    for e in entries:
        by_sha.setdefault(e["sha256"], e)
    names = {e["name"] for e in entries}
    for pattern in EXCLUDED:
        for p in (x.strip() for x in re.split(r"[,/]", pattern)):
            rx = re.compile("^" + re.escape(p).replace(r"\*", ".*") + "$")
            hit = [n for n in names if rx.match(n)]
            if hit:
                fail("excluded prebuilt entry in the needed set: %s" % hit)

    out = {
        "archives": sorted(by_sha.values(), key=lambda e: e["name"]),
        "goModules": gomods,
        "rust": rust,
        "excluded": EXCLUDED,
    }
    text = json.dumps(out, indent=2, sort_keys=True) + "\n"
    if args.output == "-":
        sys.stdout.write(text)
    else:
        open(args.output, "w").write(text)


if __name__ == "__main__":
    main()
