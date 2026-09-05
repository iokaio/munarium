#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""The license gate for Munarium: one command over the whole repository.

    py check_license.py

Checks, in order:

  1. The root texts. LICENSE is byte-for-byte the canonical Apache-2.0 text
     (sha256-pinned below, compared LF-normalized so a Windows checkout passes),
     and NOTICE, TRADEMARK.md and CODE_OF_CONDUCT.md exist and are non-empty.
  2. Every component's own gate, run in its own directory.

Why the second half delegates rather than inlines: each component's checker
knows things the others do not -- the server and Matrix rule on a Cargo
workspace and its members, while the clients rule on four packaging ecosystems,
built artifacts and `cargo package --list`. Folding three different sets of
domain knowledge into one file would mean rewriting all three, and the value is
in what they check rather than in how many files do it. This is the one gate a
contributor runs; those are the three it is made of.

Exit 1 if any of them fails, naming which. Stdlib only.
"""
from __future__ import annotations

import hashlib
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
APACHE_2_0_SHA256 = "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30"
TEXTS = ("LICENSE", "NOTICE", "TRADEMARK.md", "CODE_OF_CONDUCT.md")
COMPONENTS = ("server", "matrix", "clients")


def lf(raw: bytes) -> bytes:
    return raw.replace(b"\r\n", b"\n")


def check_root_texts(bad: list[str]) -> None:
    for name in TEXTS:
        p = ROOT / name
        if not p.is_file() or p.stat().st_size == 0:
            bad.append(f"{name}: missing or empty")
    lic = ROOT / "LICENSE"
    if lic.is_file():
        got = hashlib.sha256(lf(lic.read_bytes())).hexdigest()
        if got != APACHE_2_0_SHA256:
            bad.append(f"LICENSE is not the canonical Apache-2.0 text (sha256 {got}); it is never edited")


def run_component(name: str, bad: list[str]) -> None:
    d = ROOT / name
    gate = d / "check_license.py"
    if not gate.is_file():
        bad.append(f"{name}/check_license.py: missing")
        return
    r = subprocess.run([sys.executable, "check_license.py"], cwd=d, capture_output=True, text=True)
    out = (r.stdout + r.stderr).strip()
    print(f"  {name}: " + (out.splitlines()[-1] if out else "(no output)"))
    if r.returncode != 0:
        for line in out.splitlines():
            bad.append(f"{name}: {line}")


def main() -> int:
    bad: list[str] = []
    check_root_texts(bad)
    for name in COMPONENTS:
        run_component(name, bad)
    if bad:
        for b in bad:
            print(f"FAIL: {b}")
        return 1
    print("check_license: root texts canonical, all three components ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
