#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""The §18.3 measurement-discipline lint.

    A number quoted in any document of this set comes from a
    matrix/conformance/results/<run-id>.json whose fingerprint is quoted
    beside it.

The half of that rule a script can check: every cycle a document names must
have its results file committed. A cycle id is eight characters of
[a-z0-9], and the documents always introduce one with the word "cycle" (or
"cycles", or "run id"), so the scan is: after that word, every backticked
eight-character token within the same sentence. Anything else that happens to
be eight characters — a word, a hash prefix — is not preceded by "cycle" and
is not a cycle.

Cycles that predate the results file (the first sixteen ran before
a live run wrote one) are listed in `conformance/results/UNRECORDED`, one
id per line with a reason, and pass. Adding an id there is a statement that
the number beside it cannot be replayed; it is not a way to make the lint
quiet.

Exit 1 on any cycle id with neither a results file nor an UNRECORDED entry.
Stdlib only, so it runs wherever `test.ps1` and CI do.
"""
from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
REPO = ROOT.parent

# The documents the rules cover: everything Markdown under matrix/ (skipping
# untracked dependency and build trees — ui-smoke's node_modules carries
# hundreds of package READMEs).
_SKIP_DIRS = {"node_modules", "target"}
SOURCES = sorted(p for p in ROOT.rglob("*.md") if not _SKIP_DIRS & set(p.parts))
RESULTS = ROOT / "conformance" / "results"

# The second rule: every relative link in these documents resolves. A first
# sweep found fourteen broken relative links under matrix/ — source files linked at a depth written
# for another folder — and nothing that would have caught them. Fenced code is
# skipped; http(s), mailto and anchor-only links are skipped; a fragment is
# ignored; %20 is decoded.
LINK = re.compile(r"\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
FENCE = re.compile(r"```.*?```", re.S)


def broken_links(path: pathlib.Path, text: str) -> list[str]:
    import urllib.parse

    out: list[str] = []
    for m in LINK.finditer(FENCE.sub("", text)):
        target = m.group(1).strip("<>")
        if target.startswith(("http://", "https://", "mailto:", "#")):
            continue
        rel = urllib.parse.unquote(target.split("#", 1)[0])
        if rel and not (path.parent / rel).exists():
            out.append(target)
    return out

# "cycle `abcd1234`", "cycles `a` and `b`", "cycles 19 and 20 (`a`, `b`)",
# "run id `abcd1234`". One sentence: stop at a period followed by whitespace,
# or at a blank line.
TRIGGER = re.compile(r"\b(?:[Cc]ycles?|run id)\b")
ID = re.compile(r"`([a-z0-9]{8})`")
SENTENCE_END = re.compile(r"\.\s|\n\s*\n")


def cycle_ids(text: str) -> set[str]:
    found: set[str] = set()
    for m in TRIGGER.finditer(text):
        tail = text[m.end():]
        end = SENTENCE_END.search(tail)
        window = tail[: end.start()] if end else tail[:400]
        for token in ID.findall(window):
            # A cycle id has at least one digit: `bri5eayn`, `64n4bcvi`. An
            # all-letter token after "cycle" is a word in backticks.
            if any(c.isdigit() for c in token):
                found.add(token)
    return found


def main() -> int:
    recorded = {p.stem for p in RESULTS.glob("*.json")}
    unrecorded: dict[str, str] = {}
    listing = RESULTS / "UNRECORDED"
    if listing.exists():
        for line in listing.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            cid, _, reason = line.partition(" ")
            unrecorded[cid] = reason.strip()

    failures: list[tuple[str, str]] = []
    cited: set[str] = set()
    for path in SOURCES:
        if not path.exists():
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        for cid in sorted(cycle_ids(text)):
            cited.add(cid)
            if cid in recorded or cid in unrecorded:
                continue
            failures.append((str(path.relative_to(REPO)), cid))

    for rel, cid in failures:
        print(f"{rel}: cycle `{cid}` has no conformance/results/{cid}.json and no UNRECORDED entry")
    orphans = sorted(recorded - cited)
    if orphans:
        # Informational: a results file nobody cites is not wrong, but it is
        # a measurement nobody has written down.
        print(f"note: {len(orphans)} results file(s) cited by no document: {', '.join(orphans)}")

    dead_links: list[tuple[str, str]] = []
    links = 0
    for path in SOURCES:
        if not path.exists():
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        links += len(LINK.findall(FENCE.sub("", text)))
        for target in broken_links(path, text):
            dead_links.append((str(path.relative_to(REPO)), target))
    for rel, target in dead_links:
        print(f"{rel}: link `{target}` names nothing that exists")

    if failures or dead_links:
        print(f"doclint: {len(failures)} uncited cycle id(s), {len(dead_links)} dead link(s)")
        return 1
    print(
        f"doclint: {len(cited)} cycle id(s) across {len(SOURCES)} document(s), "
        f"{len(recorded)} results files, {len(unrecorded)} declared unrecorded; "
        f"{links} links, none dead — ok"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
