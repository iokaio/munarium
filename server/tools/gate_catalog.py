# SPDX-License-Identifier: Apache-2.0
"""Portable gate commands shared by local receipts and independent CI jobs."""

import argparse
import json
import re
import shutil
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
CATALOG = Path(__file__).with_name("gate-catalog.json")


def load(path=CATALOG):
    data = json.loads(path.read_text(encoding="utf-8"))
    if data["schema_version"] != 1 or not data["steps"]:
        raise ValueError("unsupported or empty catalog")
    seen = set()
    for step in data["steps"]:
        if not step["id"] or step["id"] in seen:
            raise ValueError("duplicate or empty step id")
        if step["required"] is not True:
            raise ValueError("gates must remain required")
        if any(dep not in seen for dep in step["depends_on"]):
            raise ValueError("missing, cyclic or out-of-order dependency")
        if step["cwd"] not in (".", "server"):
            raise ValueError("unsupported working directory")
        if not step["commands"] or not step["prerequisites"]:
            raise ValueError("missing commands or prerequisites")
        for command in step["commands"]:
            if not command or not all(isinstance(arg, str) and arg for arg in command):
                raise ValueError("invalid argument array")
        seen.add(step["id"])
    return data


def execute(step, root=ROOT, run=subprocess.run):
    for tool in step["prerequisites"]:
        if tool != "py" and not shutil.which(tool):
            raise RuntimeError(f"{step['id']}: required tool unavailable: {tool}")
    for command in step["commands"]:
        argv = [sys.executable if arg == "{python}" else arg for arg in command]
        result = run(argv, cwd=root / step["cwd"], check=False)
        if result.returncode:
            raise RuntimeError(f"{step['id']}: command failed ({result.returncode})")


def dependency_names(output, crate):
    lines = output.splitlines()
    # A successful but empty/malformed cargo result is not a dependency proof.
    if not lines or any(not re.match(r"^[\w-]+ v\d", line) for line in lines):
        raise ValueError("empty or malformed cargo tree")
    names = {line.split()[0] for line in lines}
    if crate not in names:
        raise ValueError("cargo tree omitted the requested crate")
    return names


def boundaries(kind, root=ROOT, run=subprocess.run):
    server = root / "server"
    policy = load()["boundaries"]
    if kind in ("crates", "datastore"):
        for crate, rule in policy.items():
            if (crate == "munarium-datastore") != (kind == "datastore"):
                continue
            result = run(
                [
                    "cargo",
                    "tree",
                    "-p",
                    crate,
                    "-e",
                    "normal",
                    "--prefix",
                    "none",
                    *rule["features"],
                ],
                cwd=server,
                capture_output=True,
                text=True,
                check=True,
            )
            names = dependency_names(result.stdout, crate)
            bad = names.intersection(rule["banned"])
            if "workspace_allow" in rule:
                bad |= {name for name in names if name.startswith("munarium-")} - set(
                    rule["workspace_allow"]
                )
            if bad:
                raise ValueError(f"boundary violation: {crate}: {sorted(bad)}")
    elif kind == "retrieval":
        source = server / "src/munarium-server/src"
        files = [path for path in source.rglob("*") if path.is_file()]
        if not files or not (source / "state.rs").is_file():
            raise ValueError("retrieval source tree missing")
        for path in files:
            if (
                path != source / "state.rs"
                and "munarium_retrieval_pg" in path.read_text(encoding="utf-8")
            ):
                raise ValueError(
                    f"retrieval boundary violation: {path.relative_to(root)}"
                )
    elif kind == "migrations":
        files = [
            path
            for path in (server / "src/munarium-store-pg/migrations").rglob("*")
            if path.is_file()
        ]
        if not files:
            raise ValueError("migration tree missing")
        for path in files:
            if re.search(
                r"^\s*(drop\s+(table|column)|alter\s+table\s+\S+\s+drop)",
                path.read_text(encoding="utf-8"),
                re.IGNORECASE | re.MULTILINE,
            ):
                raise ValueError(f"destructive DDL: {path.relative_to(root)}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("ids", nargs="*")
    parser.add_argument(
        "--boundary", choices=["crates", "datastore", "retrieval", "migrations"]
    )
    args = parser.parse_args()
    try:
        if args.boundary:
            boundaries(args.boundary)
        else:
            steps = {step["id"]: step for step in load()["steps"]}
            if not args.ids or any(key not in steps for key in args.ids):
                raise ValueError("select known, nonempty gate IDs")
            completed = set()

            def visit(key):
                if key in completed:
                    return
                for dep in steps[key]["depends_on"]:
                    visit(dep)
                execute(steps[key])
                completed.add(key)

            for key in args.ids:
                visit(key)
    except (ValueError, RuntimeError, OSError, subprocess.CalledProcessError) as error:
        print(str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
