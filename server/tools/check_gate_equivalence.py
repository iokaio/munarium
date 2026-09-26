# SPDX-License-Identifier: Apache-2.0
"""Verify the reviewed P16 catalog and all untouched CI orchestration bytes.

This intentionally fails closed when CI evolves: review and update the baseline
alongside an intentional coverage change. It is not a general YAML parser or a
proof that a command's implementation has the claimed semantics.
"""

import hashlib
import json
import sys
from pathlib import Path

from gate_catalog import ROOT, load

BASELINE = Path(__file__).with_name("gate-ci-baseline.json")


def check(workflow, catalog, baseline):
    # Pin identities, commands/feature flags, cwd, prerequisite and dependency
    # closure, required status, adapters, and each recursive boundary's policy.
    if catalog != baseline["catalog"]:
        raise ValueError("catalog differs from reviewed coverage inventory")
    masked = workflow
    for name, entry in baseline["steps"].items():
        ids = entry["ids"]
        if name == "fmt":
            ids = [
                "runner.regression",
                "catalog.regression",
                "catalog.equivalence",
                *ids,
            ]
        replacement = f"      - name: {name}\n        run: python3 tools/gate_catalog.py {' '.join(ids)}\n"
        if masked.count(replacement) != 1:
            raise ValueError(f"changed or missing CI requirement: {name}")
        masked = masked.replace(replacement, f"      # catalog: {name}\n", 1)
    if hashlib.sha256(masked.encode()).hexdigest() != baseline["preserved_sha256"]:
        raise ValueError(
            "unmigrated CI inventory or orchestration changed; audit the baseline"
        )
    if (ROOT / "AGENTS.md").read_bytes() != (ROOT / "CLAUDE.md").read_bytes():
        raise ValueError("AGENTS.md and CLAUDE.md differ")


def main():
    try:
        baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
        check(
            (ROOT / ".github/workflows/server-ci.yml").read_text(encoding="utf-8"),
            load(),
            baseline,
        )
    except (ValueError, OSError) as error:
        print(error, file=sys.stderr)
        return 1
    print(
        "CI inventory preserved: identities, commands, features, dependencies, required status; agent guidance identical"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
