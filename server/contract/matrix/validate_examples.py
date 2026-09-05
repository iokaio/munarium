#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Validate every committed contract example against its JSON Schema.

Run by `matrix/test.ps1` on every push and by both trees' CI. The point is not
that the examples are pretty: they are the fixtures BOTH trees deserialize, so
an example that stops matching its schema is a contract break caught here
rather than in an integration run.

Reads files as bytes and decodes UTF-8 explicitly. A console redirect on
Windows adds a BOM, and the server tree already lost an afternoon to that.
"""
from __future__ import annotations

import json
import pathlib
import sys

HERE = pathlib.Path(__file__).parent

# example filename -> schema filename
PAIRS = {
    "query-intent.structured.json": "query-intent.schema.json",
    "evidence-manifest.table.json": "evidence-manifest.schema.json",
    "evidence-block.complete-table.json": "evidence-block.schema.json",
    "evidence-block.count.json": "evidence-block.schema.json",
    "evidence-block.refusal.json": "evidence-block.schema.json",
    "refusal.policy-denied.json": "refusal.schema.json",
    "refusal.hidden-required-layer.json": "refusal.schema.json",
    "observation-batch.json": "observation-batch.schema.json",
}


def load(path: pathlib.Path):
    return json.loads(path.read_bytes().decode("utf-8"))


def main() -> int:
    try:
        import jsonschema
        from referencing import Registry, Resource
    except ImportError:
        print("contract: jsonschema is not installed; skipping (pip install jsonschema)")
        return 0

    schemas = {p.name: load(p) for p in HERE.glob("*.schema.json")}
    registry = Registry()
    for name, schema in schemas.items():
        resource = Resource.from_contents(schema)
        registry = registry.with_resource(name, resource)
        if "$id" in schema:
            registry = registry.with_resource(schema["$id"], resource)

    failures = 0

    # The schemas themselves must be valid before they can judge anything.
    for name, schema in schemas.items():
        try:
            jsonschema.Draft202012Validator.check_schema(schema)
        except Exception as exc:  # noqa: BLE001 - report and continue
            failures += 1
            print(f"SCHEMA INVALID {name}: {exc}")

    examples = {p.name for p in (HERE / "examples").glob("*.json")}
    unpaired = examples - set(PAIRS)
    if unpaired:
        failures += 1
        print(f"examples with no schema pairing: {sorted(unpaired)}")

    for example, schema_name in PAIRS.items():
        path = HERE / "examples" / example
        if not path.exists():
            failures += 1
            print(f"MISSING example {example}")
            continue
        validator = jsonschema.Draft202012Validator(schemas[schema_name], registry=registry)
        errors = sorted(validator.iter_errors(load(path)), key=lambda e: list(e.path))
        if errors:
            failures += 1
            print(f"FAIL {example} vs {schema_name}")
            for err in errors[:5]:
                print(f"    at {list(err.path)}: {err.message[:200]}")
        else:
            print(f"ok   {example}")

    version = (HERE / "VERSION").read_bytes().decode("utf-8").strip()
    if not version:
        failures += 1
        print("VERSION is empty")
    else:
        print(f"ok   contract version {version}")

    if failures:
        print(f"\n{failures} contract failure(s)")
        return 1
    print("\ncontract ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
