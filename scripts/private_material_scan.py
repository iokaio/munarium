#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""The private-material gate: what must never re-enter this repository.

README.md says the private research and development behind Munarium -- the
experiments, the measurements, the superseded designs, the operational records
of the environments they ran in -- is deliberately not carried here. That
sentence is a promise, and a promise nothing checks is a promise that decays.
This is the check.

It is deliberately crude. A rule that can be argued with is a rule that gets
argued with at 6pm on a release day, so each one is a literal pattern with a
name, and the way to permit an occurrence is to write it down in
`scripts/private_material_scan.allow` with a reason. Adding a line there is a
statement that the occurrence is deliberate and publishable. It is not a way
to make the scanner quiet.

Two rules deserve their own note, because they cover the two things a text
scan would otherwise miss and a human reviewer slides straight past:

  * ALT_TEXT reads the alt text of every Markdown image. The measurement that
    survived longest in this tree was not in prose -- it was in the alt text
    of `ch19-patterns-map.png`, where a reader's eye never lands.
  * IMAGE_SOURCE refuses a raster under `docs/**/images/` that has no
    committed source beside it. A scanner cannot read a PNG, so the fix is
    structural: ship diagrams as text, and the text is scannable.

    py scripts/private_material_scan.py

Exit 1 on any finding, naming the file, line, rule and matched text. Stdlib
only, and no network.
"""
from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
ALLOW_FILE = ROOT / "scripts" / "private_material_scan.allow"

SKIP_DIRS = {
    "target", "build", "bin", "obj", ".git", ".gradle", "node_modules",
    "__pycache__", ".venv", "dist", ".idea", ".vs", "TestResults",
}
TEXT_SUFFIXES = {
    ".rs", ".py", ".md", ".toml", ".yml", ".yaml", ".json", ".sql", ".ps1",
    ".sh", ".kts", ".gradle", ".cs", ".java", ".proto", ".txt", ".tf",
    ".tfvars", ".props", ".csproj", ".dockerfile", ".env", ".svg", ".mmd",
}
TEXT_NAMES = {"Dockerfile", "NOTICE", "LICENSE", "CODEOWNERS", ".gitattributes"}

# ---------------------------------------------------------------- the rules

RULES: list[tuple[str, re.Pattern[str], str]] = [
    ("LEGACY_NAMES",
     re.compile(r"\b(mmesh|mmeshctl|MMESH_|x-mmesh-|Muninn Mesh|MeshError)\b", re.I),
     "the product's retired name, or a type named after it"),

    ("LAB_VOCAB",
     re.compile(r"\b(the lab's|the lab\b|research instrument|graded corp(us|ora)|"
                r"white paper|measured live|live-verified|experiment harness|"
                r"experimental harness|owner-directed)\b", re.I),
     "private research vocabulary"),

    ("PRIVATE_PLANS",
     re.compile(r"(commercial repository plan|research workspace|phase \d+ step)", re.I),
     "an internal planning document"),

    # The codenames inside the corpora, and the snake_case ids of the
    # experiments that ran over them. The sample runbooks are kebab-case
    # (`threat-intelligence`); the snake_case form only ever named an
    # experiment, so it has no legitimate use in this tree.
    ("PRIVATE_CORPORA",
     re.compile(r"\b(SILKNOTE|TIDEGLASS|COPPERVEIL|GRAYFROST|Vale Family|"
                r"Aster Peak|Northgate Systems|"
                r"threat_intelligence|patent_analysis|support_knowledge|"
                r"financial_advisory|due_diligence|history_revolution|"
                r"regulatory_compliance|legal_contracts|legal_appeal|"
                r"insurance_claims|customer_support|sweep_coverage|sweepv2)\b"),
     "a private corpus, or the experiment that ran over it"),

    # A bare score or a bare price is not a leak: a published cloud list price
    # and a SQL `$0.00` literal are both fine, and so is "7/7 in the mysql
    # tier", which anyone can reproduce from this tree with docker compose.
    # What leaks is a score or a cost ATTRIBUTED to a model or a graded run, so
    # both halves have to be on the line. A leading hyphen or digit excludes
    # dates like 2026-08-17/19.
    ("MEASUREMENTS",
     re.compile(r"((?<![-\d])\b\d{1,3}/\d{1,3}\b[^\n]{0,70}?\b(sonnet|haiku|gpt-\d|"
                r"glm-|model class|both models|graded|answer key)\b"
                r"|\b(sonnet|haiku|gpt-\d|glm-|graded)\b[^\n]{0,70}?(?<![-\d])\b\d{1,3}/\d{1,3}\b"
                r"|\brecall\s+0\.\d+"
                r"|\bfinding_recall\b"
                r"|\bF1\s+[01]\.\d+"
                r"|\$\d+\.\d{2}\b[^\n]{0,50}?\b(sonnet|haiku|gpt-\d|glm-|per sweep|"
                r"per run|the run|both models)\b)", re.I),
     "a measured result attributed to a model or a graded run"),

    ("LIVE_ENV_VARS",
     re.compile(r"MUNARIUM_MATRIX_(LIVE_DATABRICKS|LIVE_DBT|TEST_SNOWFLAKE|"
                r"TEST_BIGQUERY|TEST_CUBE)"),
     "a private live-deployment variable"),
]

# Markdown image alt text: ![alt](path)
IMG = re.compile(r"!\[([^\]]*)\]\(([^)]+)\)")
ALT_RULES = {"LAB_VOCAB", "MEASUREMENTS", "PRIVATE_CORPORA", "LEGACY_NAMES"}


def load_allow() -> set[tuple[str, str]]:
    """`path:rule` per line, with a reason after a second colon."""
    allow: set[tuple[str, str]] = set()
    if not ALLOW_FILE.exists():
        return allow
    for raw in ALLOW_FILE.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(":", 2)
        if len(parts) < 3 or not parts[2].strip():
            sys.exit(f"{ALLOW_FILE.name}: '{line}' needs path:RULE:reason")
        allow.add((parts[0].strip().replace("\\", "/"), parts[1].strip()))
    return allow


def is_text(p: pathlib.Path) -> bool:
    return p.suffix.lower() in TEXT_SUFFIXES or p.name in TEXT_NAMES


def walk() -> list[pathlib.Path]:
    out = []
    for p in ROOT.rglob("*"):
        if not p.is_file() or SKIP_DIRS & set(p.parts):
            continue
        out.append(p)
    return out


def main() -> int:
    allow = load_allow()
    findings: list[str] = []

    for p in walk():
        rel = p.relative_to(ROOT).as_posix()

        # Structural: a raster with no committed source beside it.
        if p.suffix.lower() in {".png", ".jpg", ".jpeg", ".gif", ".webp"} \
                and "/images/" in f"/{rel}":
            if ("IMAGE_SOURCE" not in {r for pa, r in allow if pa == rel}
                    and not any(p.with_suffix(s).exists() for s in (".svg", ".mmd", ".dot", ".puml"))):
                findings.append(
                    f"{rel}: IMAGE_SOURCE: a raster with no committed .svg/.mmd source "
                    "beside it -- a scanner cannot read what a diagram says")
            continue

        if not is_text(p):
            continue
        try:
            text = p.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue

        exempt = {r for pa, r in allow if pa == rel}
        for n, line in enumerate(text.splitlines(), 1):
            for name, pattern, why in RULES:
                if name in exempt:
                    continue
                m = pattern.search(line)
                if m:
                    findings.append(f"{rel}:{n}: {name}: {why} -- {m.group(0)!r}")
            if p.suffix == ".md":
                for alt, _path in IMG.findall(line):
                    for name, pattern, why in RULES:
                        if name in exempt or name not in ALT_RULES:
                            continue
                        m = pattern.search(alt)
                        if m:
                            findings.append(
                                f"{rel}:{n}: ALT_TEXT/{name}: {why}, in image alt text "
                                f"-- {m.group(0)!r}")

    for f in findings:
        print(f"::error::{f}" if "CI" in __import__("os").environ else f"FAIL: {f}")
    if findings:
        print(f"\nprivate_material_scan: {len(findings)} finding(s). Either remove the "
              f"material, or record the exception in {ALLOW_FILE.name} with a reason.")
        return 1

    print(f"private_material_scan: clean across {len(walk())} files "
          f"({len(RULES)} rules + alt-text + image-source; "
          f"{len(allow)} reasoned exception(s))")
    return 0


if __name__ == "__main__":
    sys.exit(main())
