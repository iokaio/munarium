#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Check the vendored Matrix contract against a standalone Matrix checkout."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
VENDORED = ROOT / "server" / "contract" / "matrix"


def matrix_root() -> Path:
    configured = os.environ.get("MUNARIUM_MATRIX_ROOT")
    candidates = [Path(configured)] if configured else []
    candidates.extend((ROOT / "munarium-matrix", ROOT.parent / "munarium-matrix"))
    for candidate in candidates:
        if (candidate / "contract" / "publish.py").is_file():
            return candidate.resolve()
    locations = ", ".join(str(candidate.resolve()) for candidate in candidates)
    raise FileNotFoundError(
        "Munarium Matrix contract publisher not found. Clone "
        "https://github.com/iokaio/munarium-matrix beside this repository, "
        "or set MUNARIUM_MATRIX_ROOT. Checked: " + locations
    )


def check(source: Path, runner=subprocess.run) -> None:
    publisher = source / "contract" / "publish.py"
    runner([sys.executable, str(publisher), "--self-test"], check=True)
    runner([sys.executable, str(publisher), "--check", str(VENDORED)], check=True)


def main() -> int:
    try:
        source = matrix_root()
        check(source)
    except (FileNotFoundError, subprocess.CalledProcessError) as error:
        print(error, file=sys.stderr)
        return 1
    version = (source / "contract" / "VERSION").read_text(encoding="utf-8").strip()
    print(f"Matrix contract {version}: publisher self-test and vendored-copy check passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
