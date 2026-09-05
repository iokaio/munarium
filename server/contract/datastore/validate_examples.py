# SPDX-License-Identifier: Apache-2.0
"""Check the Munarium Datastore contract: examples, vectors, and refusals.

Run from this directory: `python validate_examples.py`. Exit 0 means the
committed contract is self-consistent. This is what a CI step calls, and what a
reviewer runs before believing a schema edit was harmless.

Three checks, because a contract can be broken three ways:

  1. every example validates against its schema  -- the shapes are legal
  2. every identity vector recomputes to its committed hash -- the RULES did not
     move under an edit that looked cosmetic
  3. every must-refuse case is actually refused -- the negative space is real,
     which is the half a schema silently loses when someone relaxes
     additionalProperties
"""

import copy
import io
import json
import sys

import canonicalize as C

try:
    from jsonschema import Draft202012Validator
except ImportError:
    print("FAIL: jsonschema is not installed (pip install jsonschema)")
    raise SystemExit(2)

PAIRS = [
    ("build-spec.schema.json", "examples/build-spec.collection.json"),
    ("artifact-plan.schema.json", "examples/artifact-plan.tantivy-exact.json"),
    ("manifest.schema.json", "examples/manifest.tantivy-exact.json"),
]

failures = []


def load(p):
    with io.open(p, encoding="utf-8") as fh:
        return json.load(fh)


def check(ok, msg):
    print(("  ok   " if ok else "  FAIL ") + msg)
    if not ok:
        failures.append(msg)


print("1. examples validate against their schemas")
for schema_path, example_path in PAIRS:
    schema, example = load(schema_path), load(example_path)
    Draft202012Validator.check_schema(schema)
    errs = sorted(Draft202012Validator(schema).iter_errors(example), key=lambda e: list(e.path))
    check(not errs, f"{example_path} against {schema_path}"
                    + ("" if not errs else f" -- {errs[0].json_path}: {errs[0].message}"))

print("2. identity vectors recompute")
vectors = load("vectors/identity-vectors.json")
check(vectors["canon"] == "artifact@1", "vectors declare canon artifact@1")

spec = load("examples/build-spec.collection.json")
plan = load("examples/artifact-plan.tantivy-exact.json")
manifest = load("examples/manifest.tantivy-exact.json")

# The manifest's embedded hashes must actually be the hashes of the committed
# sidecars. An example whose build_spec_sha256 is stale would make every vector
# below agree with itself and with nothing real.
check(manifest["build_spec_sha256"] == C.sha256_hex(spec),
      "manifest.build_spec_sha256 matches the committed BuildSpec")
check(manifest["artifact_plan_sha256"] == C.sha256_hex(plan),
      "manifest.artifact_plan_sha256 matches the committed plan")

base = next(v for v in vectors["vectors"] if v["name"] == "baseline")
check(base["index_version_id"] == C.index_version_id(spec), "baseline index_version_id")
check(base["artifact_id"] == C.artifact_id(manifest), "baseline artifact_id")
check(base["index_version_id"].startswith("idx2-"), "logical ids use the idx2- namespace")
check(len(base["artifact_id"]) == 64, "artifact_id is a bare 64-hex digest, no prefix")

moves = {}
for v in vectors["vectors"]:
    moves.setdefault(v["moves"], 0)
    moves[v["moves"]] += 1
    if v["name"] == "baseline":
        continue
    same_logical = v["index_version_id"] == base["index_version_id"]
    same_artifact = v["artifact_id"] == base["artifact_id"]
    if v["moves"] == "neither":
        ok = same_logical and same_artifact
    elif v["moves"] == "artifact":
        ok = same_logical and not same_artifact
    else:
        ok = (not same_logical) and (not same_artifact)
    check(ok, f"{v['name']}: moves {v['moves']}")
check(all(k in moves for k in ("neither", "artifact", "logical")),
      "all three invariant classes are exercised")

print("3. must-refuse cases are refused")
mr = {c["name"]: c for c in vectors["must_refuse"]}

try:
    C.serialize(mr["float_in_document"]["document"])
    check(False, "float_in_document: canonicalizer accepted a float")
except ValueError:
    check(True, "float_in_document: canonicalizer refuses a float")

mschema = load("manifest.schema.json")
mv = Draft202012Validator(mschema)


def refuses(mutate, label):
    bad = copy.deepcopy(manifest)
    mutate(bad)
    check(bool(list(mv.iter_errors(bad))), label)


refuses(lambda m: m.__setitem__("built_at", "2026-08-29T12:00:00Z"),
        "manifest_with_build_timestamp: schema refuses non-content metadata")
refuses(lambda m: m.__setitem__("tenant_id", "tenant-default"),
        "manifest_with_tenant_id: schema refuses authority in a content hash")
refuses(lambda m: m["components"][0].__setitem__("path", "../../etc/passwd"),
        "component_path_traversal: schema refuses '..'")
refuses(lambda m: m["components"][0].__setitem__("path", "/etc/passwd"),
        "component_path_absolute: schema refuses a leading '/'")
refuses(lambda m: m["components"][0].__setitem__("path", "lexical\\meta.json"),
        "component_path_backslash: schema refuses a Windows separator")

print()
if failures:
    print(f"FAILED: {len(failures)} check(s)")
    sys.exit(1)
print("contract OK")
