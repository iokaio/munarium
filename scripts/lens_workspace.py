# SPDX-License-Identifier: Apache-2.0
"""Freeze and verify the Git inputs for the LENS-D2 workspace exercise."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
from collections import Counter
from pathlib import Path, PurePosixPath
from typing import Any

ROOTS = frozenset({
    "munarium", "munarium-matrix", "munarium-lens", "munarium-platform",
    "munarium-assure", "munarium-console", "munarium-council", "munarium-gate",
    "munarium-gateway", "munarium-harness", "munarium-registry", "munarium-sentinel",
    "munarium-warden", "munarium-clients-publish", "munarium-demo", "munarium-int",
})
RESTRICTED = "munarium-int"
OID = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})\Z")
DOCS = {".md", ".rst", ".adoc", ".txt"}
SOURCE = {
    ".c", ".h", ".cpp", ".rs", ".py", ".sh", ".ps1", ".psm1", ".bat", ".cmd",
    ".cs", ".csproj", ".sln", ".slnx", ".java", ".kt", ".gradle", ".go", ".js",
    ".mjs", ".ts", ".tsx", ".jsx", ".vue", ".css", ".html", ".json", ".jsonl",
    ".yaml", ".yml", ".toml", ".ini", ".xml", ".sql", ".proto", ".tf", ".tpl",
    ".lock", ".mod", ".sum", ".properties", ".csv", ".tsv", ".ipynb", ".cshtml",
    ".pyi", ".kts", ".cjs", ".props", ".lockfile", ".psd1", ".dockerfile",
}
MEDIA = {
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".svg", ".zip",
    ".jar", ".wasm", ".woff", ".woff2", ".eot", ".ttf", ".tif", ".tiff",
}
SOURCE_NAMES = {
    "dockerfile", "makefile", "gradlew", ".gitignore", ".gitattributes", ".dockerignore",
    "codeowners", "py.typed",
}
DOC_NAMES = {"license", "notice", "readme", "changelog", "support", "third_party_notices"}
POINTER = b"version https://git-lfs.github.com/spec/v1"


def git(root: Path, *args: str, input_bytes: bytes | None = None) -> bytes:
    return subprocess.run(
        ["git", "--no-replace-objects", "-c", f"safe.directory={root.as_posix()}",
         "-C", str(root), *args],
        input=input_bytes, check=True, stdout=subprocess.PIPE,
    ).stdout


def tree_entries(data: bytes) -> list[dict[str, Any]]:
    files = []
    if data and not data.endswith(b"\0"):
        raise ValueError("Git tree listing has an unterminated entry")
    for record in data.split(b"\0")[:-1]:
        metadata, raw_path = record.split(b"\t", 1)
        mode, kind, oid, size = metadata.decode("ascii").split()
        if not OID.fullmatch(oid):
            raise ValueError("invalid Git object ID")
        path = raw_path.decode("utf-8")
        if any(part in {"", ".", ".."} for part in path.split("/")):
            raise ValueError(f"invalid tree path {path!r}")
        files.append({"path": path, "mode": mode, "type": kind, "oid": oid,
                      "size": None if size == "-" else int(size)})
    if len({entry["path"] for entry in files}) != len(files):
        raise ValueError("duplicate Git tree path")
    return files


def lfs_pointers(root: Path, files: list[dict[str, Any]]) -> set[str]:
    candidates = {
        f["oid"] for f in files
        if f["mode"] in {"100644", "100755"} and len(POINTER) <= f["size"] <= 1024
    }
    if not candidates:
        return set()
    query = "".join(f"{oid}\n" for oid in sorted(candidates)).encode("ascii")
    data = git(root, "cat-file", "--batch", input_bytes=query)
    offset, pointers = 0, set()
    for expected in sorted(candidates):
        end = data.index(b"\n", offset)
        oid, kind, size = data[offset:end].decode("ascii").split()
        start, offset = end + 1, end + 2 + int(size)
        if oid != expected or kind != "blob" or data[offset - 1:offset] != b"\n":
            raise ValueError("unexpected Git blob response")
        if data[start:offset - 1].startswith((POINTER + b"\n", POINTER + b"\r\n")):
            pointers.add(oid)
    if offset != len(data):
        raise ValueError("unexpected trailing Git blob data")
    return pointers


def classify(entry: dict[str, Any], pointers: set[str]) -> tuple[str, str]:
    mode = entry["mode"]
    if mode == "120000":
        return "excluded", "symlink"
    if mode == "160000":
        return "review", "gitlink"
    if mode not in {"100644", "100755"} or entry["type"] != "blob":
        return "review", "unexpected_tree_entry"
    if entry["oid"] in pointers:
        return "review", "large_file_pointer"
    path = PurePosixPath(entry["path"].lower())
    if path.name.startswith("i3_") or entry["path"] in {
        "scripts/test_i3_eval.py", "server/docs/guides/frozen-evaluation.md",
    }:
        return "excluded", "evaluation_material"
    if path.suffix in DOCS or path.name in DOC_NAMES:
        return "docs", "documentation"
    if path.suffix in SOURCE or path.name in SOURCE_NAMES:
        return "code", "source_or_build_input"
    if path.suffix in MEDIA:
        return "excluded", "media_or_binary_asset"
    if path.suffix in {".pdf", ".docx", ".xlsx"}:
        return "review", "document_extraction_profile"
    return "review", "unclassified_path"


def freeze(workspace: Path, pins: dict[str, str], restricted_authority: str | None) -> dict[str, Any]:
    if not pins or not pins.keys() <= ROOTS:
        raise ValueError("pin at least one documented workspace root")
    if RESTRICTED in pins and not restricted_authority:
        raise ValueError("restricted root requires explicit corpus authority before inspection")
    roots = {}
    for name, commit in sorted(pins.items()):
        if not OID.fullmatch(commit):
            raise ValueError(f"{name} requires a full Git commit ID")
        root = workspace / name
        if root.resolve() != root:
            raise ValueError(f"{name} is not a direct workspace checkout")
        actual_root = Path(git(root, "rev-parse", "--show-toplevel").decode().strip())
        if actual_root.resolve() != root:
            raise ValueError(f"{name} is nested in another Git checkout")
        if git(root, "rev-parse", "--verify", f"{commit}^{{commit}}").decode().strip() != commit:
            raise ValueError(f"{name} pin does not resolve to the given commit")
        tree = git(root, "rev-parse", f"{commit}^{{tree}}").decode().strip()
        files = tree_entries(git(root, "ls-tree", "-r", "-l", "-z", commit))
        pointers = lfs_pointers(root, files)
        counts: Counter[str] = Counter()
        byte_counts: Counter[str] = Counter()
        for entry in files:
            family, reason = classify(entry, pointers)
            entry.update(family=family, reason=reason)
            counts[family] += 1
            byte_counts[family] += entry["size"] or 0
        roots[name] = {
            "commit": commit, "tree": tree, "access": "restricted" if name == RESTRICTED else "public",
            "counts": dict(counts), "bytes": dict(byte_counts), "files": files,
        }
    document = {
        "format": "lens-workspace-git-1", "roots": roots, "missing_roots": sorted(ROOTS - roots.keys()),
        "unclassified_entries": sum(root["counts"].get("review", 0) for root in roots.values()),
    }
    if RESTRICTED in pins:
        document["restricted_authority"] = restricted_authority
    return document


def canonical(document: dict[str, Any]) -> bytes:
    return (json.dumps(document, sort_keys=True, indent=2, ensure_ascii=True) + "\n").encode("utf-8")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("freeze", "verify"))
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--workspace", required=True, type=Path)
    parser.add_argument("--pin", action="append", default=[], metavar="ROOT=COMMIT")
    parser.add_argument("--restricted-authority", help="Opaque corpus authority record for a restricted run")
    args = parser.parse_args()
    if args.operation == "freeze":
        pins = dict(pin.split("=", 1) for pin in args.pin)
        if len(pins) != len(args.pin):
            parser.error("duplicate root pin")
    else:
        if args.pin:
            parser.error("verify obtains pins from the manifest")
        manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
        pins = {name: root["commit"] for name, root in manifest["roots"].items()}
    document = freeze(args.workspace.resolve(), pins, args.restricted_authority)
    data = canonical(document)
    if args.operation == "freeze":
        with args.manifest.open("xb") as output:
            output.write(data)
    elif document != manifest:
        raise SystemExit("Git tree and classification differ from the supplied manifest")
    print(json.dumps({
        "sha256": hashlib.sha256(data).hexdigest(), "roots": len(pins),
        "missing_roots": document["missing_roots"], "unclassified_entries": document["unclassified_entries"],
    }))


if __name__ == "__main__":
    main()
