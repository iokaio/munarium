# SPDX-License-Identifier: Apache-2.0
"""Generate `vectors/identity-vectors.json` from the committed examples.

The vectors are the executable half of `canonicalization.schema.json`. The schema
says what the rules are; these say what they PRODUCE, on documents that exist, so
a Rust implementation can be checked against a number instead of against a
paragraph.

Each vector is a named mutation of the baseline plus the assertion the section
5.1 invariant makes about it:

    neither  -- non-content metadata changed; no identifier moves
    artifact -- engine, revision or envelope changed; artifact_id ONLY
    logical  -- corpus, chunker, extractor, analyzer or embedder changed;
                index_version_id moves (and artifact_id with it, since the
                manifest embeds the spec hash)

Regenerate with `python gen_vectors.py`; check with `python validate_examples.py`.
"""

import copy
import io
import json

import canonicalize as C

BASE_SPEC = "examples/build-spec.collection.json"
BASE_PLAN = "examples/artifact-plan.tantivy-exact.json"
BASE_MANIFEST = "examples/manifest.tantivy-exact.json"


def load(p):
    with io.open(p, encoding="utf-8") as fh:
        return json.load(fh)


def ids(spec, plan, manifest):
    """Recompute the manifest's embedded hashes, as a real builder would."""
    m = copy.deepcopy(manifest)
    m["build_spec_sha256"] = C.sha256_hex(spec)
    m["artifact_plan_sha256"] = C.sha256_hex(plan)
    return {
        "index_version_id": C.index_version_id(spec),
        "artifact_plan_sha256": C.sha256_hex(plan),
        "artifact_id": C.artifact_id(m),
    }


def main():
    spec, plan, manifest = load(BASE_SPEC), load(BASE_PLAN), load(BASE_MANIFEST)
    base = ids(spec, plan, manifest)

    cases = []

    def case(name, moves, why, spec_m=None, plan_m=None, man_m=None):
        s, p, m = copy.deepcopy(spec), copy.deepcopy(plan), copy.deepcopy(manifest)
        if spec_m:
            spec_m(s)
        if plan_m:
            plan_m(p)
        if man_m:
            man_m(m)
        got = ids(s, p, m)
        moved = set()
        if got["index_version_id"] != base["index_version_id"]:
            moved.add("index_version_id")
        if got["artifact_id"] != base["artifact_id"]:
            moved.add("artifact_id")
        expected = {
            "neither": set(),
            "artifact": {"artifact_id"},
            "logical": {"index_version_id", "artifact_id"},
        }[moves]
        assert moved == expected, f"{name}: expected {moves} ({expected}), moved {moved}"
        cases.append({"name": name, "moves": moves, "why": why, **got})

    case("baseline", "neither", "the unmutated example; every other vector is measured against it")

    # --- artifact-only: physical realization changed, logical corpus did not ---
    case("lexical_engine_revision", "artifact",
         "an engine upgrade is a new physical artifact of the SAME logical version -- this is the "
         "property that lets a session pin survive an engine promotion",
         plan_m=lambda p: p["lexical"].__setitem__("engine_revision", "0.23.0"))
    case("envelope_format_version", "artifact",
         "the envelope format is physical; a reader-compatibility change must not force a reindex",
         plan_m=lambda p: p["envelope"].__setitem__("format_version", 2))
    case("vector_kind_approximate", "artifact",
         "exact-vs-approximate is a binding change, recorded in the plan and never in the "
         "logical id",
         plan_m=lambda p: (p["vector"].__setitem__("kind", "approximate"),
                           p["vector"].__setitem__("engine_id", "diskann")))

    # --- logical: something that changes results ---
    case("source_content_hash", "logical",
         "different bytes in a source are a different corpus",
         spec_m=lambda s: s["sources"][0].__setitem__("content_sha256", "9" * 64))
    case("source_order", "logical",
         "source order is part of the identity: the builder streams in this order, so two builds "
         "of the same SET that disagree by permutation are not the same build",
         spec_m=lambda s: s.__setitem__("sources", list(reversed(s["sources"]))))
    case("chunker_param", "logical",
         "a resolved chunker parameter changes the chunks, so it changes results",
         spec_m=lambda s: s["chunker"]["params"].__setitem__("max_chars", 900))
    case("extractor_outcome", "logical",
         "per-source extracted text is what got indexed; without it two builds whose extraction "
         "silently differed would share a logical id",
         spec_m=lambda s: s["extractor"]["per_source"][1].__setitem__("extracted_text_sha256", "7" * 64))
    case("analyzer_tokenizer", "logical",
         "the section 5.1 V1 decision in force: the analyzer contract is a result-affecting input, "
         "so a tokenizer change produces a new logical version that must be activated like any "
         "corpus change. The corpus-only alternative is a documented design choice.",
         spec_m=lambda s: s["lexical_analysis"].__setitem__("tokenizer", "munarium-pg-compat@2"))
    case("analyzer_stop_terms", "logical",
         "the stop list is carried by HASH, not by reference alone -- a reference would let the "
         "list change under a fixed logical id",
         spec_m=lambda s: s["lexical_analysis"]["stop_terms_ref"].__setitem__("sha256", "8" * 64))
    case("embedder_model", "logical",
         "a different embedder ranks differently; the provenance envelope's index_version is what "
         "an audit uses to say how an answer was ranked",
         spec_m=lambda s: s["embedder"].__setitem__("model", "local-hash@2"))
    case("embedder_absent", "logical",
         "a lexical-only corpus is valid and is NOT the same logical corpus as a hybrid one",
         spec_m=lambda s: s.__setitem__("embedder", None))
    case("snapshot_watermark", "logical",
         "the same sources at a different watermark is a different snapshot",
         spec_m=lambda s: s["snapshot"].__setitem__("watermark_seq", 148214))

    # --- neither: canonicalization must absorb these ---
    case("key_order_permuted", "neither",
         "the same content with object members written in a different order canonicalizes to the "
         "SAME bytes -- this is the whole point of JCS key ordering",
         spec_m=lambda s: s.__setitem__("scope", {"id": s["scope"]["id"], "kind": s["scope"]["kind"]}))
    case("probe_order_is_content", "artifact",
         "ARRAY order is content, unlike object member order: reordering probes is a different "
         "manifest, because an array is a sequence and canonicalization does not sort it",
         man_m=lambda m: m.__setitem__("probes", list(reversed(m["probes"]))))

    out = {
        "canon": "artifact@1",
        "generated_from": {"spec": BASE_SPEC, "plan": BASE_PLAN, "manifest": BASE_MANIFEST},
        "note": (
            "Regenerate with gen_vectors.py. A Rust implementation of artifact@1 must reproduce "
            "every hash below byte for byte. Where the two disagree, THESE are the contract and "
            "both implementations are suspect until one is shown to violate the schema."
        ),
        "vectors": cases,
        "must_refuse": [
            {"name": "float_in_document",
             "why": "artifact@1 forbids floating-point numbers; a canonicalizer must refuse rather "
                    "than format one. See canonicalization.schema.json why_no_floats.",
             "document": {"spec_version": 1, "ratio": 0.5}},
            {"name": "manifest_with_build_timestamp",
             "why": "the manifest is content-pure. A builder that stamps built_at makes byte-identical "
                    "rebuilds collide instead of converging, which is the defect the purity rule exists "
                    "to prevent. additionalProperties:false must reject it.",
             "document_note": "the baseline manifest plus \"built_at\": \"2026-08-29T12:00:00Z\""},
            {"name": "manifest_with_tenant_id",
             "why": "tenant is authority, not content. An identical corpus in two tenants legitimately "
                    "produces one artifact_id; isolation lives in the catalog key and the runtime "
                    "ArtifactCacheKey. Putting it here would make a content hash pretend to be an "
                    "authorization boundary.",
             "document_note": "the baseline manifest plus \"tenant_id\": \"tenant-default\""},
            {"name": "component_path_traversal",
             "why": "component paths are normalized relative paths; '..' escapes the artifact prefix",
             "document_note": "the baseline manifest with a component path of \"../../etc/passwd\""},
            {"name": "component_path_absolute",
             "why": "an absolute path ignores the artifact root entirely",
             "document_note": "the baseline manifest with a component path of \"/etc/passwd\""},
        ],
    }
    io.open("vectors/identity-vectors.json", "w", encoding="utf-8", newline="\n").write(
        json.dumps(out, indent=2, ensure_ascii=False) + "\n"
    )
    print(f"wrote vectors/identity-vectors.json  ({len(cases)} vectors, "
          f"{len(out['must_refuse'])} refusal cases)")
    for c in cases:
        print(f"  {c['moves']:9s} {c['name']}")


if __name__ == "__main__":
    main()
