#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Cut the Munarium Matrix cross-tree contract as a locked, vendorable bundle.

    py contract/publish.py --out <dir>        # cut the contract into <dir> (created; must be empty)
    py contract/publish.py --verify <dir>     # every file in <dir> matches its contract.lock
    py contract/publish.py --check <dir>      # <dir> is what THIS tree would cut (drift check)
    py contract/publish.py --self-test        # two cuts of this tree are byte-identical

Run from `matrix/`, or from anywhere: paths are resolved from this file.

`matrix/contract/` is the boundary between the Matrix and Server trees (ground rule 1:
no crate dependency in either direction), and `server/contract/matrix/` is its vendored
copy. Until 2026-09-03 the copy was proven by `diff -r` against this directory — a check
that needs both trees in one checkout, which a standalone Server repository does not
have. This publisher is the
replacement, on the pattern of server/contract/mmp/publish.py:

    contract.lock     the contract version, the source commit, a sha256 per file and a
                      digest over the sorted list -- written into the cut, never copied
    every other file  this directory, verbatim: VERSION, the schemas, examples/,
                      README.md, validate_examples.py (the contract's own gate)

Two proofs replace the one diff. Where the sibling tree exists (this repository's CI,
`--check`), the vendored copy must equal a fresh cut, source commit ignored. Where it does
not (a standalone Server checkout), `munarium-api-types`' `matrix_contract` test verifies
every vendored file against the lock and refuses anything unlisted -- the same rule as
`--verify`, in the language the server tree already tests in.

Reproducibility rules: every file is written UTF-8, LF, no BOM, whatever the checkout's
line endings are, so a cut on Windows and a cut on Linux are the same bytes. The publisher
itself is not part of the contract and is never copied. Stdlib only. Exit 1 on any
verification or drift failure.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent          # matrix/contract
MATRIX = HERE.parent                            # matrix/
NOT_CONTRACT = {"publish.py", "contract.lock"}  # the publisher, and the lock it writes
BUNDLE = "munarium-matrix-contract"


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


def is_build_product(rel: Path) -> bool:
    """Interpreter caches a developer creates by running the gate in place."""
    return "__pycache__" in rel.parts


def contract_files() -> list[str]:
    out = []
    for p in sorted(HERE.rglob("*")):
        rel = p.relative_to(HERE)
        if p.is_file() and p.name not in NOT_CONTRACT and not is_build_product(rel):
            out.append(rel.as_posix())
    return out


def source_commit() -> str:
    try:
        sha = subprocess.run(["git", "rev-parse", "HEAD"], cwd=MATRIX, capture_output=True, text=True, check=True).stdout.strip()
        dirty = subprocess.run(["git", "status", "--porcelain", "--", "contract"], cwd=MATRIX, capture_output=True, text=True, check=True).stdout.strip()
        return sha + ("+dirty" if dirty else "")
    except Exception:  # noqa: BLE001 - a cut outside git still records something true
        return "unknown"


def digest_of(files: dict[str, str]) -> str:
    return hashlib.sha256("".join(f"{h}  {n}\n" for n, h in sorted(files.items())).encode()).hexdigest()


def cut(out: Path) -> None:
    out.mkdir(parents=True, exist_ok=True)
    if any(out.iterdir()):
        raise SystemExit(f"--out {out}: directory is not empty")
    version = text(HERE / "VERSION").strip()
    for rel in contract_files():
        put(out, rel, text(HERE / rel))
    files = {}
    for p in sorted(out.rglob("*")):
        if p.is_file():
            files[p.relative_to(out).as_posix()] = hashlib.sha256(p.read_bytes()).hexdigest()
    lock = {
        "bundle": BUNDLE,
        "contract_version": version,
        "source_commit": source_commit(),
        "bundle_digest": digest_of(files),
        "files": files,
    }
    put(out, "contract.lock", json.dumps(lock, indent=2, sort_keys=True))


def read_lock(d: Path) -> dict:
    p = d / "contract.lock"
    if not p.exists():
        raise SystemExit(f"{d}: no contract.lock -- cut it with `py matrix/contract/publish.py --out {d}`")
    return json.loads(text(p))


def verify(d: Path) -> int:
    lock = read_lock(d)
    bad = 0
    seen = set()
    for p in sorted(d.rglob("*")):
        rel_path = p.relative_to(d)
        if not p.is_file() or p.name == "contract.lock" or is_build_product(rel_path):
            continue
        rel = rel_path.as_posix()
        seen.add(rel)
        h = hashlib.sha256(p.read_bytes()).hexdigest()
        if rel not in lock["files"]:
            print(f"UNLISTED {rel}"); bad += 1
        elif lock["files"][rel] != h:
            print(f"MODIFIED {rel}"); bad += 1
    for rel in lock["files"]:
        if rel not in seen:
            print(f"MISSING  {rel}"); bad += 1
    if digest_of(lock["files"]) != lock["bundle_digest"]:
        print("bundle_digest does not match the file list"); bad += 1
    print(f"verify {d}: {len(lock['files'])} files, contract {lock['contract_version']}; problems: {bad}")
    return 1 if bad else 0


def compare(a: Path, b: Path, *, ignore_commit: bool) -> int:
    def listing(d: Path) -> dict:
        out = {}
        for p in sorted(d.rglob("*")):
            rel_path = p.relative_to(d)
            if p.is_file() and not is_build_product(rel_path):
                rel = rel_path.as_posix()
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
        print(f"check {d}: {bad} difference(s) from what this tree cuts -- re-cut it: "
              f"rm -r {d}; py matrix/contract/publish.py --out {d}")
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
        print(f"cut {a.out}: {len(lock['files'])} files, contract {lock['contract_version']}, "
              f"bundle_digest {lock['bundle_digest'][:16]}..., source {lock['source_commit']}")
        return 0
    if a.verify:
        return verify(a.verify.resolve())
    if a.check:
        return check(a.check.resolve())
    return self_test()


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
