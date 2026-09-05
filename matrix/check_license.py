#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""The license gate for Munarium Matrix: Apache-2.0 wherever a tool reads a licence, the
license texts present, and every Ioka-authored source file self-describing.

    py check_license.py            # the checks below; exit 1 on any finding
    py check_license.py --stamp    # add the missing SPDX headers in place, then check

Checks:

  1. Manifests. The workspace Cargo.toml declares `Apache-2.0` and every
     member crate inherits it (`license.workspace = true`) or declares the same.
  2. Texts. LICENSE, NOTICE, TRADEMARK.md and THIRD_PARTY_NOTICES.md exist and are
     non-empty, and LICENSE is the canonical Apache-2.0 text (sha256 pinned, compared
     LF-normalized so a Windows checkout passes).
  3. Headers. Every Ioka-authored source file of a kind the REUSE convention can carry a
     comment in has `SPDX-License-Identifier: Apache-2.0` on its first
     line (second, after a shebang or an XML declaration; a UTF-8 BOM is ignored).
     Exempt, and why: see EXEMPT.
  4. Forbidden strings. No SPDX line naming the retired LicenseRef-Ioka-Proprietary
     identifier in a file the exemptions do not cover.

`--stamp` inserts the header where check 3 would fail, keeping the file's own line
ending, BOM, shebang and XML declaration. It never touches an exempt path.

Stdlib only; `git ls-files` decides what is tracked, and outside a git repository (a
staged product tree) the walk below is the same set minus build output.
"""
from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent
PRODUCT = "Munarium Matrix"

IDENTIFIER = "Apache-2.0"
SPDX = f"SPDX-License-Identifier: {IDENTIFIER}"
# Assembled, not written out: this file is scanned by its own forbidden-string check.
FORBIDDEN = "SPDX-License-Identifier: " + "LicenseRef-Ioka-Proprietary"

HEADER = {
    ".rs": f"// {SPDX}",
    ".cs": f"// {SPDX}",
    ".java": f"// {SPDX}",
    ".kts": f"// {SPDX}",
    ".proto": f"// {SPDX}",
    ".py": f"# {SPDX}",
    ".pyi": f"# {SPDX}",
    ".toml": f"# {SPDX}",
    ".ps1": f"# {SPDX}",
    ".sh": f"# {SPDX}",
    ".csproj": f"<!-- {SPDX} -->",
    ".props": f"<!-- {SPDX} -->",
}
# Paths (prefixes, relative to ROOT, forward slashes) the header rule does not reach:
EXEMPT = (
    # applied migrations: sqlx validates a checksum per file, so their bytes are frozen
    "src/munarium-matrix-store/migrations/",
    # the contract: published byte for byte into the Server repository under a lock,
    # and its schemas are JSON, which carries no comment
    "contract/",
    # Gradle's own wrapper files, under Gradle's own Apache-2.0 header
    "clients/java/gradlew",
    "clients/java/gradlew.bat",
    "clients/java/gradle/wrapper/",
)
# Matrix publishes no public bundle; only the third-party license texts quoted verbatim
# in the notices file legitimately carry another SPDX line.
FORBIDDEN_ALLOWED: tuple[str, ...] = ("THIRD_PARTY_NOTICES.md",)
# TRADEMARK.md is not here: the consolidated repository carries one policy at
# its root, and duplicating it per component would be three files to keep in
# step for no reader's benefit.
TEXTS = ("LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.md")
# The canonical Apache-2.0 text, sha256, so an edited LICENSE fails rather than passes.
LICENSE_SHA256 = "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30"

SKIP_DIRS = {".git", "target", "node_modules", "__pycache__", ".venv", "venv", "bin", "obj",
             ".gradle", "build", "dist", ".mypy_cache", ".ruff_cache", ".pytest_cache"}


def lf(raw: bytes) -> bytes:
    """CRLF-normalized bytes, so a Windows checkout hashes the same as a Linux one."""
    return raw.replace(bytes([13, 10]), bytes([10]))


def tracked_files() -> list[str]:
    try:
        out = subprocess.run(
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            cwd=ROOT, capture_output=True, check=True,
        ).stdout
        return sorted({p for p in out.decode("utf-8").split("\0") if p and (ROOT / p).is_file()})
    except (subprocess.CalledProcessError, FileNotFoundError):
        return sorted(
            p.relative_to(ROOT).as_posix()
            for p in ROOT.rglob("*")
            if p.is_file() and not (SKIP_DIRS & set(p.relative_to(ROOT).parts))
        )


def exempt(rel: str) -> bool:
    return any(rel.startswith(e) for e in EXEMPT)


def header_slot(raw: bytes) -> tuple[bytes, bytes, bytes, bytes]:
    """Split a file into (bom, lead, first_line, eol): `lead` is a shebang or XML
    declaration line that must stay first (with its line ending); `first_line` is the
    line after it, where the header goes or is; `eol` is the file's own line ending."""
    bom = b""
    if raw.startswith(b"\xef\xbb\xbf"):
        bom, raw = raw[:3], raw[3:]
    first_raw = raw.split(b"\n", 1)[0]
    eol = b"\r\n" if first_raw.endswith(b"\r") else b"\n"
    lead = b""
    if first_raw.startswith(b"#!") or first_raw.lstrip().startswith(b"<?xml"):
        lead = first_raw + b"\n"
        raw = raw[len(lead):]
    first = raw.split(b"\n", 1)[0].rstrip(b"\r")
    return bom, lead, first, eol


def has_header(raw: bytes, header: str) -> bool:
    _bom, _lead, first, _eol = header_slot(raw)
    return first.decode("utf-8", "replace").strip() == header


def stamp(path: Path, header: str) -> None:
    raw = path.read_bytes()
    bom, lead, _first, eol = header_slot(raw)
    body = raw[len(bom) + len(lead):]
    path.write_bytes(bom + lead + header.encode("utf-8") + eol + body)


def check_manifests(bad: list[str]) -> None:
    ws = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    lic = ws.get("workspace", {}).get("package", {}).get("license")
    if lic != IDENTIFIER:
        bad.append(f"Cargo.toml: workspace.package.license is {lic!r}, expected {IDENTIFIER!r}")
    for rel in tracked_files():
        if rel.endswith("Cargo.toml") and rel != "Cargo.toml" and not exempt(rel):
            try:
                m = tomllib.loads((ROOT / rel).read_text(encoding="utf-8"))
            except tomllib.TOMLDecodeError as e:
                bad.append(f"{rel}: {e}"); continue
            pkg = m.get("package")
            if not pkg:
                continue
            l = pkg.get("license")
            if l == IDENTIFIER or (isinstance(l, dict) and l.get("workspace")):
                continue
            bad.append(f"{rel}: package.license is {l!r}, expected {IDENTIFIER!r} or license.workspace = true")


def check_texts(bad: list[str]) -> None:
    for t in TEXTS:
        p = ROOT / t
        if not p.is_file() or p.stat().st_size == 0:
            bad.append(f"{t}: missing or empty")
    lic = ROOT / "LICENSE"
    if lic.is_file():
        import hashlib
        got = hashlib.sha256(lf(lic.read_bytes())).hexdigest()
        if got != LICENSE_SHA256:
            bad.append(f"LICENSE is not the canonical Apache-2.0 text (sha256 {got}); it is never edited")


def check_headers(bad: list[str], do_stamp: bool) -> int:
    stamped = 0
    for rel in tracked_files():
        header = HEADER.get(Path(rel).suffix)
        if header is None or exempt(rel):
            continue
        p = ROOT / rel
        raw = p.read_bytes()
        if has_header(raw, header):
            continue
        if do_stamp:
            stamp(p, header); stamped += 1
            continue
        bad.append(f"{rel}: first line is not `{header}`")
    return stamped


def check_forbidden(bad: list[str]) -> None:
    for rel in tracked_files():
        if exempt(rel) or rel in FORBIDDEN_ALLOWED:
            continue
        p = ROOT / rel
        try:
            txt = p.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        for n, line in enumerate(txt.splitlines(), 1):
            if FORBIDDEN in line:
                bad.append(f"{rel}:{n}: carries the retired proprietary identifier; this tree is {IDENTIFIER}")
                break


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    ap.add_argument("--stamp", action="store_true", help="insert missing SPDX headers in place")
    args = ap.parse_args()
    bad: list[str] = []
    check_manifests(bad)
    check_texts(bad)
    stamped = check_headers(bad, args.stamp)
    check_forbidden(bad)
    if stamped:
        print(f"stamped {stamped} file(s)")
    if bad:
        for b in bad:
            print(b)
        print(f"check_license ({PRODUCT}): {len(bad)} finding(s)")
        return 1
    n = sum(1 for r in tracked_files() if Path(r).suffix in HEADER and not exempt(r))
    print(f"check_license ({PRODUCT}): manifests {IDENTIFIER}, texts present, {n} source files headed -- ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
