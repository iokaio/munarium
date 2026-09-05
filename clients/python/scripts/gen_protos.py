#!/usr/bin/env python
# SPDX-License-Identifier: Apache-2.0
"""Regenerate the committed gRPC stubs under src/munarium_client/_proto.

    python scripts/gen_protos.py

Reads the ten normative protos from server/proto/mmp/v1 (the tree that owns
them), rewrites the generated imports to be package-relative, and emits
deterministic output for the CI drift check
(`git diff --exit-code src/munarium_client/_proto`).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

from grpc_tools import protoc

ROOT = Path(__file__).resolve().parent.parent
# The normative protos, in the tree that owns them.
PROTO_ROOT = ROOT.parent.parent / "server" / "proto"
OUT = ROOT / "src" / "munarium_client" / "_proto"

PROTOS = [
    "common",
    "ledger",
    "command",
    "query",
    "retrieval",
    "ingest",
    "runbook",
    "provider",
    "session",
    "admin",
]


def main() -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    files = [str(PROTO_ROOT / "mmp" / "v1" / f"{name}.proto") for name in PROTOS]
    # grpc_tools bundles the well-known types (google/protobuf/*.proto).
    well_known = Path(protoc.__file__).resolve().parent / "_proto"
    args = [
        "protoc",
        f"-I{PROTO_ROOT}",
        f"-I{well_known}",
        f"--python_out={OUT}",
        f"--pyi_out={OUT}",
        f"--grpc_python_out={OUT}",
        *files,
    ]
    if protoc.main(args) != 0:
        print("protoc failed", file=sys.stderr)
        return 1

    # Package inits so `from munarium_client._proto.mmp.v1 import ...` resolves.
    for pkg in [OUT, OUT / "mmp", OUT / "mmp" / "v1"]:
        init = pkg / "__init__.py"
        if not init.exists():
            init.write_text("", encoding="utf-8")

    # grpc_tools emits absolute `from mmp.v1 import ...` imports; rewrite them
    # relative to this package so installation needs no sys.path games.
    for py in (OUT / "mmp" / "v1").glob("*.py*"):
        text = py.read_text(encoding="utf-8")
        text = re.sub(
            r"^from mmp\.v1 import ",
            "from munarium_client._proto.mmp.v1 import ",
            text,
            flags=re.M,
        )
        py.write_text(text, encoding="utf-8", newline="\n")

    print(f"generated {len(files)} protos into {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
