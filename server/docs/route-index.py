#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Regenerate the developers guide's Appendix F route index from the served
OpenAPI document.

    cargo run -q -p munarium-server -- openapi > docs/api/openapi.json   # first, if routes changed
    python docs/route-index.py                                            # prints the table

Paste the output over the table under "### Appendix F: Route index" in
docs/guides/dev-guide.md. The `docs_coverage` tests in munarium-server fail
`cargo test` when the appendix, the REST route map (docs/api/rest.md) or the
error registry (docs/api/errors.md) fall behind the code, which is what keeps
the book complete for the API after 2026-09-02.

The chapter column is a prefix table below; add a rule when a new route
family gets a chapter. Unmatched routes point at rest.md, which the coverage
test already forces to be complete.
"""
import json
import os

HERE = os.path.dirname(os.path.abspath(__file__))
OPENAPI = os.path.join(HERE, "api", "openapi.json")

CHAPTERS = [
    ("/v1/max-tokens", "§20 (spend governance); docs/tokenbudgets.md"),
    ("/v1/reports/budgets", "§20 (spend governance)"),
    ("/v1/reports/", "§20; rest.md Reports rows"),
    ("/v1/collections/{collection_id}/activate-index", "§8A"),
    ("/v1/index-artifacts", "§8A"),
    ("/v1/index-build-jobs", "§8A"),
    ("/v1/retrieval-rollout", "§8A"),
    ("/v1/authoring", "§21B"),
    ("/v1/evidence", "§21C"),
    ("/v1/sessions", "§17"),
    ("/v1/runbooks", "§21A"),
    ("/v1/runs", "§21A"),
    ("/v1/access-tokens", "§20"),
    ("/v1/providers", "§11 (BYOK diagnostic), §17"),
    ("/healthai", "§11"),
    ("/v1/chronology-rules", "§18"),
    ("/v1/shapes", "§16"),
    ("/v1/collections", "§16"),
    ("/v1/indexes", "§16"),
    ("/v1/search", "§16"),
    ("/v1/ingest", "§15, §21"),
    ("/v1/sources", "§6, §15"),
    ("/v1/versions", "§5, §6 (recipes), §18"),
    ("/v1/claims", "§18"),
    ("/healthz", "§5, §11"),
    ("/readyz", "§5, §8A (datastore readiness), §11"),
    ("/version", "§5"),
]


def chapter(path: str) -> str:
    for prefix, where in CHAPTERS:
        if path == prefix or path.startswith(prefix + "/") or (prefix.endswith("/") and path.startswith(prefix)):
            return where
    return "rest.md"


def main() -> None:
    with open(OPENAPI, encoding="utf-8-sig") as f:
        doc = json.load(f)
    print("| Route | Methods | OpenAPI tag | Where this book teaches it |")
    print("|---|---|---|---|")
    for path, item in sorted(doc["paths"].items()):
        methods = [m.upper() for m in ("get", "post", "put", "delete", "patch") if m in item]
        tags = sorted({t for m in methods for t in item[m.lower()].get("tags", [])})
        print(f"| `{path}` | {' · '.join(methods)} | {', '.join(tags) or '—'} | {chapter(path)} |")


if __name__ == "__main__":
    main()
