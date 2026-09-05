#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Cut the public MMP contract bundle from this server tree.

    py contract/mmp/publish.py --out <dir>        # cut the bundle into <dir> (created; must be empty)
    py contract/mmp/publish.py --verify <dir>     # every file in <dir> matches its contract.lock
    py contract/mmp/publish.py --check <dir>      # <dir> is what THIS tree would cut (drift check)
    py contract/mmp/publish.py --self-test        # two cuts of this tree are byte-identical

Run from `server/`, or from anywhere: paths are resolved from this file.

The bundle is the interface subset of the server that the public Clients
repository needs to build and prove compatibility (
phase2-clients-inventory.md §4). Both it and the server tree it is
cut from are Apache-2.0; the publisher still stamps the license in as it copies,
so a bundle file declares its own license rather than inheriting one. Source of
truth stays here. Nothing in a bundle is hand-edited.

What it carries, and how each part is produced:

    VERSION                      mmp: v1 / server: <workspace version>; no commit, no date
    contract.lock                source commit, sha256 per file, a digest over all of them
    README.md                    contract/mmp/README.md, verbatim
    LICENSE, NOTICE              contract/mmp/LICENSE (Apache-2.0, verbatim) and NOTICE
    proto/mmp/v1/*.proto         proto/mmp/v1/*.proto, verbatim
    openapi.json                 docs/api/openapi.json, verbatim (the generated document CI drift-checks)
    errors.md                    docs/api/errors.md, verbatim (the problem-slug registry)
    rust/munarium-api-types/     src/munarium-api-types: Cargo.toml concretized (workspace
                                 inheritance resolved, license Apache-2.0, dev-dependencies
                                 dropped), src/lib.rs + src/wire.rs with an SPDX header;
                                 tests/ excluded (they read the vendored Matrix contract)
    rust/munarium-proto/         src/munarium-proto: Cargo.toml concretized, build.rs +
                                 src/lib.rs with an SPDX header; src/bin/ excluded (the
                                 grpc-reference generator is a server docs tool)
    conformance/SCENARIOS.md     conformance/SCENARIOS.md, verbatim (the eight scenario contracts)

Reproducibility rules: every text file is written UTF-8, LF, no BOM, whatever
the checkout's line endings are (a Windows checkout with autocrlf carries CRLF
in the protos), so a cut on Windows and a cut on Linux are the same bytes.
Nothing time- or host-dependent enters a file; the source commit lives only in
contract.lock and is excluded from the drift comparison, because the vendored
copy in Clients records the commit it was cut at, not the tree checking it.

Stdlib only. Exit 1 on any verification or drift failure.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent          # server/contract/mmp
SERVER = HERE.parent.parent                     # server/
PROTOS = ["common", "ledger", "command", "query", "retrieval", "ingest", "runbook", "provider", "admin", "session"]
SPDX = "// SPDX-License-Identifier: Apache-2.0\n"
PUBLIC_LICENSE = "Apache-2.0"
# The canonical copies of the two wire crates carry their own SPDX line. The cut removes
# whichever line is there and prepends the bundle's own, so the bundle's bytes do not
# depend on what the canonical file declares. Match ANY SPDX line, never one identifier:
# matching a single value is how a relicense once made this emit two headers instead of
# one. The bundle's bytes do not change when the canonical header does, which is the point.
CANONICAL_SPDX_PREFIX = "// SPDX-License-Identifier: "


def public_source(src: str) -> str:
    """A wire-crate source file as the bundle publishes it: whatever SPDX line the
    canonical tree carries (if any) removed, the bundle's own line prepended."""
    if src.startswith(CANONICAL_SPDX_PREFIX):
        src = src.split("\n", 1)[1] if "\n" in src else ""
    return SPDX + src
MMP_VERSION = "v1"


def text(path: Path) -> str:
    """A file's text, normalized to LF and stripped of a BOM."""
    raw = path.read_bytes()
    if raw.startswith(b"\xef\xbb\xbf"):
        raw = raw[3:]
    return raw.decode("utf-8").replace("\r\n", "\n")


def put(out: Path, rel: str, content: str) -> None:
    p = out / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    if not content.endswith("\n"):
        content += "\n"
    p.write_bytes(content.encode("utf-8"))


# --- Cargo.toml concretization -------------------------------------------------------

def _toml_str(s: str) -> str:
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _dep_spec(name: str, spec, workspace_deps: dict) -> str:
    """One dependency line with workspace inheritance resolved."""
    extra: dict = {}
    if isinstance(spec, dict) and spec.get("workspace"):
        base = workspace_deps[name]
        extra = {k: v for k, v in spec.items() if k != "workspace"}
    else:
        base = spec
    if isinstance(base, str):
        merged: dict = {"version": base}
    else:
        merged = dict(base)
    merged.update(extra)
    if "path" in merged and "version" not in merged:
        pass  # a sibling crate inside the bundle keeps its relative path
    keys = ["version", "path", "features", "default-features", "optional"]
    parts = []
    for k in keys:
        if k not in merged:
            continue
        v = merged[k]
        if isinstance(v, bool):
            parts.append(f"{k} = {'true' if v else 'false'}")
        elif isinstance(v, list):
            parts.append(f"{k} = [{', '.join(_toml_str(x) for x in v)}]")
        else:
            parts.append(f"{k} = {_toml_str(str(v))}")
    unknown = set(merged) - set(keys)
    if unknown:
        raise SystemExit(f"{name}: unsupported dependency keys {sorted(unknown)}")
    if list(merged) == ["version"]:
        return f"{name} = {_toml_str(merged['version'])}"
    return f"{name} = {{ {', '.join(parts)} }}"


def concretize_manifest(crate_dir: Path, workspace: dict) -> str:
    m = tomllib.loads(text(crate_dir / "Cargo.toml"))
    pkg = m["package"]
    wpkg = workspace["workspace"]["package"]
    wdeps = workspace["workspace"]["dependencies"]
    lines = [
        f"# Generated by server/contract/mmp/publish.py from src/{crate_dir.name}/Cargo.toml.",
        "# Workspace inheritance is resolved and the license is the bundle's; do not edit.",
        "",
        "[package]",
        f"name = {_toml_str(pkg['name'])}",
        f"version = {_toml_str(wpkg['version'])}",
        f"edition = {_toml_str(wpkg['edition'])}",
        f"license = {_toml_str(PUBLIC_LICENSE)}",
        f"description = {_toml_str(pkg['description'])}",
    ]
    if "features" in m:
        lines += ["", "[features]"]
        for k, v in m["features"].items():
            lines.append(f"{k} = [{', '.join(_toml_str(x) for x in v)}]")
    for section in ("dependencies", "build-dependencies"):
        if section in m:
            lines += ["", f"[{section}]"]
            for name, spec in m[section].items():
                lines.append(_dep_spec(name, spec, wdeps))
    return "\n".join(lines) + "\n"


# --- the cut ---------------------------------------------------------------------------

def source_commit() -> str:
    try:
        sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=SERVER, capture_output=True, text=True, check=True).stdout.strip()
        dirty = subprocess.run(["git", "status", "--porcelain", "--", "."], cwd=SERVER, capture_output=True, text=True, check=True).stdout.strip()
        return sha + ("+dirty" if dirty else "")
    except Exception:  # noqa: BLE001 - a bundle cut outside git still records something true
        return "unknown"


def cut(out: Path) -> None:
    out.mkdir(parents=True, exist_ok=True)
    if any(out.iterdir()):
        raise SystemExit(f"--out {out}: directory is not empty")
    workspace = tomllib.loads(text(SERVER / "Cargo.toml"))
    server_version = workspace["workspace"]["package"]["version"]

    put(out, "VERSION", f"mmp: {MMP_VERSION}\nserver: {server_version}\n")
    put(out, "README.md", text(HERE / "README.md"))
    put(out, "LICENSE", text(HERE / "LICENSE"))
    put(out, "NOTICE", text(HERE / "NOTICE"))
    for name in PROTOS:
        put(out, f"proto/mmp/v1/{name}.proto", text(SERVER / "proto/mmp/v1" / f"{name}.proto"))
    put(out, "openapi.json", text(SERVER / "docs/api/openapi.json"))
    put(out, "errors.md", text(SERVER / "docs/api/errors.md"))
    put(out, "conformance/SCENARIOS.md", text(SERVER / "conformance/SCENARIOS.md"))

    for crate, files in (("munarium-api-types", ["src/lib.rs", "src/wire.rs"]),
                         ("munarium-proto", ["build.rs", "src/lib.rs"])):
        cdir = SERVER / "src" / crate
        put(out, f"rust/{crate}/Cargo.toml", concretize_manifest(cdir, workspace))
        for rel in files:
            put(out, f"rust/{crate}/{rel}", public_source(text(cdir / rel)))

    files = {}
    for p in sorted(out.rglob("*")):
        if p.is_file():
            files[p.relative_to(out).as_posix()] = hashlib.sha256(p.read_bytes()).hexdigest()
    digest = hashlib.sha256("".join(f"{h}  {n}\n" for n, h in sorted(files.items())).encode()).hexdigest()
    lock = {
        "bundle": "munarium-mmp-contract",
        "mmp": MMP_VERSION,
        "server": server_version,
        "license": PUBLIC_LICENSE,
        "source_commit": source_commit(),
        "bundle_digest": digest,
        "files": files,
    }
    put(out, "contract.lock", json.dumps(lock, indent=2, sort_keys=True))


# --- verification ----------------------------------------------------------------------

def read_lock(d: Path) -> dict:
    p = d / "contract.lock"
    if not p.exists():
        raise SystemExit(f"{d}: no contract.lock")
    return json.loads(text(p))


def is_build_product(rel: Path) -> bool:
    """Files a developer creates by building a vendored crate in place (`cargo check`
    in a consumer's own tree): never bundle content, never a verification failure."""
    return "target" in rel.parts or rel.name == "Cargo.lock"


def verify(d: Path) -> int:
    lock = read_lock(d)
    bad = 0
    seen = set()
    for p in sorted(d.rglob("*")):
        if not p.is_file() or p.name == "contract.lock" or is_build_product(p.relative_to(d)):
            continue
        rel = p.relative_to(d).as_posix()
        seen.add(rel)
        h = hashlib.sha256(p.read_bytes()).hexdigest()
        if rel not in lock["files"]:
            print(f"UNLISTED {rel}"); bad += 1
        elif lock["files"][rel] != h:
            print(f"MODIFIED {rel}"); bad += 1
    for rel in lock["files"]:
        if rel not in seen:
            print(f"MISSING  {rel}"); bad += 1
    digest = hashlib.sha256("".join(f"{h}  {n}\n" for n, h in sorted(lock["files"].items())).encode()).hexdigest()
    if digest != lock["bundle_digest"]:
        print("bundle_digest does not match the file list"); bad += 1
    print(f"verify {d}: {len(lock['files'])} files; problems: {bad}")
    return 1 if bad else 0


def compare(a: Path, b: Path, *, ignore_commit: bool) -> int:
    """Files and digests of two bundles; contract.lock compared without source_commit."""
    def listing(d: Path) -> dict:
        out = {}
        for p in sorted(d.rglob("*")):
            if p.is_file() and not is_build_product(p.relative_to(d)):
                rel = p.relative_to(d).as_posix()
                if rel == "contract.lock" and ignore_commit:
                    lk = json.loads(text(p)); lk.pop("source_commit", None)
                    out[rel] = hashlib.sha256(json.dumps(lk, sort_keys=True).encode()).hexdigest()
                else:
                    out[rel] = hashlib.sha256(p.read_bytes()).hexdigest()
        return out
    la, lb = listing(a), listing(b)
    bad = 0
    for rel in sorted(set(la) | set(lb)):
        if rel not in la:
            print(f"ONLY IN {b.name}: {rel}"); bad += 1
        elif rel not in lb:
            print(f"ONLY IN {a.name}: {rel}"); bad += 1
        elif la[rel] != lb[rel]:
            print(f"DIFFERS: {rel}"); bad += 1
    return bad


def check(d: Path) -> int:
    with tempfile.TemporaryDirectory() as tmp:
        fresh = Path(tmp) / "fresh"
        cut(fresh)
        bad = compare(d, fresh, ignore_commit=True)
    if bad:
        print(f"check {d}: {bad} difference(s) from what this tree cuts — re-run --out and vendor the result")
        return 1
    print(f"check {d}: identical to what this tree cuts (source commit ignored)")
    return 0


def self_test() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        a, b = Path(tmp) / "a", Path(tmp) / "b"
        cut(a); cut(b)
        bad = compare(a, b, ignore_commit=False)
        bad += verify(a)
        n = len(read_lock(a)["files"])
    if bad:
        print(f"self-test: FAILED ({bad})"); return 1
    print(f"self-test: two cuts identical, {n} files, lock verified - ok")
    return 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    g = ap.add_mutually_exclusive_group(required=True)
    g.add_argument("--out", type=Path)
    g.add_argument("--verify", type=Path)
    g.add_argument("--check", type=Path)
    g.add_argument("--self-test", action="store_true")
    a = ap.parse_args(argv)
    if a.out:
        cut(a.out.resolve())
        lock = read_lock(a.out.resolve())
        print(f"cut {a.out}: {len(lock['files'])} files, bundle_digest {lock['bundle_digest'][:16]}..., source {lock['source_commit']}")
        return 0
    if a.verify:
        return verify(a.verify.resolve())
    if a.check:
        return check(a.check.resolve())
    return self_test()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
