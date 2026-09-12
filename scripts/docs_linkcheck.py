#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""The root docs gate: every link under docs/ resolves, every page is listed.

The server's `docs_coverage` test checks relative links under server/docs,
and Matrix's doclint checks matrix/. Nothing checked the repository root or
the top-level docs/ tree, and CONTRIBUTING.md says an unlisted document is
an unread document and a dead link fails the build. This is that rule for
the part of the tree the component gates do not reach.

Two checks, both over docs/**/*.md and the root *.md files:

  * every relative link names a file or directory that exists (fenced code,
    http(s), mailto and bare #anchors are skipped; a #fragment is stripped);
  * every .md under docs/ is linked from the README.md of its own directory
    or of an ancestor directory, so the index tables stay complete.

    py scripts/docs_linkcheck.py

Exit 1 on any finding, naming the file and line. Stdlib only, no network.
"""
from __future__ import annotations

import pathlib
import re
import sys
import urllib.parse

ROOT = pathlib.Path(__file__).resolve().parents[1]
DOCS = ROOT / "docs"

# Markdown under a directory with one of these names is corpus material a
# worked example uploads, not a page anyone navigates to. Its links are still
# checked; the index rule is not applied to it.
DATA_DIR_NAMES = {"corpus"}

LINK = re.compile(r"!?\[[^\]]*\]\(<?([^)>\s]+)>?(?:\s+\"[^\"]*\")?\)")
FENCE = re.compile(r"^\s*(`{3,}|~{3,})(.*)$")


def markdown_files() -> list[pathlib.Path]:
    files = sorted(p for p in ROOT.glob("*.md") if p.is_file())
    if DOCS.is_dir():
        files += sorted(p for p in DOCS.rglob("*.md") if p.is_file())
    return files


def links_in(text: str) -> list[tuple[int, str]]:
    out: list[tuple[int, str]] = []
    fence = None
    for lineno, line in enumerate(text.splitlines(), 1):
        marker = FENCE.match(line)
        if fence is not None:
            if (marker and marker[1][0] == fence[0]
                    and len(marker[1]) >= len(fence) and not marker[2].strip()):
                fence = None
            continue
        if marker:
            fence = marker[1]
            continue
        for match in LINK.finditer(line):
            target = match.group(1)
            if target.startswith(("http://", "https://", "mailto:", "#")):
                continue
            target = urllib.parse.unquote(target.split("#", 1)[0])
            if target:
                out.append((lineno, target))
    return out


def main() -> int:
    findings: list[str] = []
    files = markdown_files()
    linked_from_index: set[pathlib.Path] = set()

    for file in files:
        text = file.read_text(encoding="utf-8", errors="replace")
        for lineno, target in links_in(text):
            resolved = (file.parent / target).resolve()
            if not resolved.exists():
                rel = file.relative_to(ROOT).as_posix()
                findings.append(f"{rel}:{lineno}: broken link -> {target}")
            elif (file.name == "README.md" and resolved.suffix == ".md"
                  and file.parent.resolve() in resolved.parents):
                linked_from_index.add(resolved)

    def is_data(path: pathlib.Path) -> bool:
        return any(part in DATA_DIR_NAMES for part in path.relative_to(ROOT).parts)

    for file in files:
        if DOCS not in file.parents or file.name == "README.md" or is_data(file):
            continue
        if file.resolve() not in linked_from_index:
            rel = file.relative_to(ROOT).as_posix()
            findings.append(f"{rel}:1: not listed from any README.md index above it")

    for directory in sorted({p.parent for p in files if (DOCS in p.parents or p.parent == DOCS) and not is_data(p)}):
        if not (directory / "README.md").is_file():
            rel = directory.relative_to(ROOT).as_posix()
            findings.append(f"{rel}/: directory under docs/ has no README.md index")

    if findings:
        for finding in findings:
            print(finding)
        print(f"docs_linkcheck: {len(findings)} finding(s)")
        return 1
    print(f"docs_linkcheck: {len(files)} markdown files, every link resolves, every page is indexed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
