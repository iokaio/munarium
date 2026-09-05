# SPDX-License-Identifier: Apache-2.0
"""The platform surface, proven through the TYPED client planes —
a port of the Rust client's platform smokes (same scenario names, in the
same order, so CI output is comparable across languages), plus the SSE
streaming-turn smokes and the gRPC-surface check.

Where the server's raw suite asserts HTTP statuses + problem slugs, this
port asserts the TYPED exceptions the client decodes them into — that
mapping is exactly what the client exists to provide. Requires the pg
store, an rw and a mgmt static token on the SAME tenant, and
MUNARIUM_TOKEN_SECRET configured server-side; skips cleanly when
MUNARIUM_MGMT_TOKEN is absent. Zero provider keys — nothing here completes.

Re-runnable against a shared dev tenant BY DESIGN: content and doomed
runbook versions are nonce'd, and no scenario asserts global tenant state
beyond what this run created. The tests are ordered: the application
scenario mints bob's token, which the SSE scenarios reuse (the same
ordering dependency the server's own suite has) — run the module whole.
"""

from __future__ import annotations

import base64
import hashlib
import os
import sys
import time
from collections.abc import Callable
from contextlib import aclosing
from pathlib import Path
from typing import Any

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

from munarium_client import (  # noqa: E402
    AsyncMunariumClient,
    ClientOptions,
    ForbiddenError,
    InvalidInputError,
    MunariumClient,
    NotFoundError,
    UnsupportedError,
    models,
)

REST_URL = os.environ.get("MUNARIUM_REST_URL")
GRPC_URL = os.environ.get("MUNARIUM_GRPC_URL")
RW_TOKEN = os.environ.get("MUNARIUM_TOKEN", "devtoken")
MGMT_TOKEN = os.environ.get("MUNARIUM_MGMT_TOKEN")

pytestmark = pytest.mark.skipif(
    REST_URL is None or MGMT_TOKEN is None,
    reason="platform smokes need MUNARIUM_REST_URL and MUNARIUM_MGMT_TOKEN",
)


def nonce() -> str:
    return f"{time.time_ns():x}"


def b64(s: str) -> str:
    return base64.b64encode(s.encode()).decode()


def sha(s: str) -> str:
    return hashlib.sha256(s.encode()).hexdigest()


SHAPE_YAML = (
    "apiVersion: munarium.ioka.io/v1\n"
    "kind: Shape\n"
    "metadata: { name: entdocs, version: 1 }\n"
    "spec:\n"
    "  fact:\n"
    "    schema: { type: object }\n"
)

# promptTemplate braces are the server's template slots, not Python format.
_RUNBOOK_TEMPLATE = """apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: ent-support, version: @VERSION@ }
spec:
  collections:
    - name: ent-public
      shape: entdocs@1
      accessLevel: 0
      sources: { filenamePrefix: "public/" }
    - name: ent-secret
      shape: entdocs@1
      accessLevel: 2
      compartments: [eng]
      sources: { filenamePrefix: "eng/" }
  retrieval: { topK: 5 }
  models:
    default: { provider: default, tier: fast }
    allowOverrides: [default]
  completion:
    promptTemplate: "Answer from context only.\\n{context}\\n\\nQ: {query}"
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verify: {}
    - cutover: { approval: required }
    - retireOld: { keep_versions: 2 }
"""


def runbook_yaml(version: int) -> str:
    return _RUNBOOK_TEMPLATE.replace("@VERSION@", str(version))


def ingest_file(name: str, text: str, collections: list[str] | None = None) -> models.IngestFile:
    return models.IngestFile(
        filename=name,
        media_type="text/markdown",
        content_base64=b64(text),
        collections=collections,
    )


def manifest_entry(name: str, text: str) -> models.BulkManifestEntry:
    return models.BulkManifestEntry(
        filename=name, sha256=sha(text), bytes_len=len(text.encode()), media_type="text/markdown"
    )


# ---------------------------------------------------------------------------
# fixtures: one pooled client per role (the connection behavior a real
# consumer has), a factory for minted personas, and the cross-test state
# the application scenario hands to the SSE scenarios.
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def ops() -> Any:
    assert REST_URL is not None
    with MunariumClient.rest(ClientOptions(REST_URL, token=RW_TOKEN, uid="ops")) as c:
        yield c


@pytest.fixture(scope="module")
def mgr() -> Any:
    assert REST_URL is not None and MGMT_TOKEN is not None
    with MunariumClient.rest(ClientOptions(REST_URL, token=MGMT_TOKEN, uid="mgr")) as c:
        yield c


@pytest.fixture(scope="module")
def make_client() -> Any:
    """One-off clients for minted personas (capability JWT + its uid),
    closed together at module teardown."""
    assert REST_URL is not None
    clients: list[MunariumClient] = []

    def make(token: str, uid: str) -> MunariumClient:
        c = MunariumClient.rest(ClientOptions(REST_URL, token=token, uid=uid))
        clients.append(c)
        return c

    yield make
    for c in clients:
        c.close()


@pytest.fixture(scope="module")
def state() -> dict[str, str]:
    return {}


def mint(mgr: MunariumClient, uid: str, level: int, comps: list[str], scopes: list[str]) -> Any:
    return mgr.tokens.mint(uid=uid, access_level=level, compartments=comps, scopes=scopes)


# ---------------------------------------------------------------------------
# scenarios
# ---------------------------------------------------------------------------


def test_uid_contract(
    mgr: MunariumClient, make_client: Callable[[str, str], MunariumClient]
) -> None:
    """The uid contract: no uid draws the typed uid-required rejection; a
    JWT presented under a different uid draws the typed 403."""
    assert REST_URL is not None
    with MunariumClient.rest(ClientOptions(REST_URL, token=RW_TOKEN)) as no_uid:
        with pytest.raises(InvalidInputError, match="uid"):
            no_uid.runbooks.list()

    grant = mint(mgr, "uid-alice", 0, [], ["query"])
    mallory = make_client(grant.token, "mallory")
    with pytest.raises(ForbiddenError):
        mallory.runbooks.list()


def test_role_partition(ops: MunariumClient, mgr: MunariumClient) -> None:
    """Role partition: rw cannot mint tokens; mgmt cannot write the ledger."""
    with pytest.raises(ForbiddenError):
        ops.tokens.mint(uid="x", access_level=0, scopes=["query"])
    with pytest.raises(ForbiddenError):
        mgr.commands.create_version()


def test_application_and_compartments(
    ops: MunariumClient,
    mgr: MunariumClient,
    make_client: Callable[[str, str], MunariumClient],
    state: dict[str, str],
) -> None:
    """The full retrieval-application lifecycle + compartmentalized
    sessions."""
    ops.runbooks.apply_shape(SHAPE_YAML)

    # Validation first: clean passes, topK: 0 invalidates.
    clean = ops.runbooks.validate(runbook_yaml(1))
    assert clean.valid, f"clean runbook must validate: {clean}"
    bad = ops.runbooks.validate(runbook_yaml(1).replace("topK: 5", "topK: 0"))
    assert not bad.valid, f"topK: 0 must invalidate: {bad}"

    ops.runbooks.apply_runbook(runbook_yaml(1))

    # Ingest via the file plane under the ingest scope; matchers auto-bind.
    loader = make_client(mint(mgr, "loader", 2, ["eng"], ["ingest"]).token, "loader")
    batch = loader.ingest.ingest_batch(
        [
            ingest_file("public/handbook.md", "The public handbook grants twenty vacation days."),
            ingest_file("eng/launch.md", "Secret launch window: vacation blackout in Q4."),
        ]
    )
    assert len(batch) == 2, f"expected 2 results: {batch}"
    assert batch[0].bound_to == ["ent-public"] and batch[1].bound_to == ["ent-secret"], (
        f"matcher auto-bind wrong: {batch}"
    )

    # A level-0 ingest token must NOT write into ent-secret.
    low = make_client(mint(mgr, "lowloader", 0, [], ["ingest"]).token, "lowloader")
    with pytest.raises(ForbiddenError):
        low.ingest.ingest(ingest_file("sneak.md", "nope", collections=["ent-secret"]))

    # Run with two per-collection approval passes.
    run = ops.runbooks.run_runbook("ent-support")
    assert run.state == "awaiting_approval", f"run must pause, got {run.state!r}"
    for _ in range(2):
        status = ops.runbooks.get_run(run.run_id)
        awaiting = next(
            (s for s in status.steps if s.state == "awaiting_approval"),
            None,
        )
        assert awaiting is not None, f"no step awaiting approval: {status}"
        ops.runbooks.approve_step(run.run_id, awaiting.ordinal)
    done = ops.runbooks.get_run(run.run_id)
    assert done.state == "done", f"run must finish done: {done}"

    # List + info expose per-collection access requirements.
    listing = ops.runbooks.list()
    entry = next((b for b in listing if b.runbook_ref == "ent-support@1"), None)
    assert entry is not None, "ent-support@1 missing from list"
    levels = [c.access_level for c in entry.collections]
    assert 0 in levels and 2 in levels, f"list must show levels 0 and 2: {levels}"
    info = ops.runbooks.get_info("ent-support")
    assert len(info.collections) == 2 and info.has_completion, (
        f"info must carry both collections + completion: {info}"
    )

    # Two clearances, one runbook: disjoint result sets for one query.
    alice = make_client(mint(mgr, "comp-alice", 0, [], ["query"]).token, "comp-alice")
    bob_token = mint(mgr, "comp-bob", 2, ["eng"], ["query"]).token
    bob = make_client(bob_token, "comp-bob")

    session_a = alice.sessions.create("ent-support")
    assert session_a.permitted_collections == ["ent-public"], (
        f"alice must see only ent-public: {session_a}"
    )
    session_b = bob.sessions.create("ent-support")
    assert len(session_b.permitted_collections) == 2, f"bob must see both: {session_b}"

    turn_a = alice.sessions.turn(session_a.session_id, query="vacation")
    assert turn_a.hits and all(h.collection == "ent-public" for h in turn_a.hits), (
        f"alice hits must be ent-public only: {turn_a}"
    )

    turn_b = bob.sessions.turn(session_b.session_id, query="vacation")
    assert any(h.collection == "ent-secret" for h in turn_b.hits), (
        f"bob's merged hits must include ent-secret: {turn_b}"
    )
    assert len(turn_b.envelopes) == 2, f"one envelope per collection: {turn_b}"

    # Multiturn continuity, transcript readback, cross-uid refusal.
    turn2 = bob.sessions.turn(session_b.session_id, query="blackout")
    assert turn2.ordinal == 2, "follow-on turn must be ordinal 2"
    readback = bob.sessions.get(session_b.session_id)
    assert len(readback.turns) == 2 and readback.state == "open", (
        f"transcript must hold both turns: {readback}"
    )
    with pytest.raises(ForbiddenError):
        alice.sessions.turn(session_b.session_id, query="x")

    # Model-override policy refusal (checked BEFORE any provider spend).
    with pytest.raises(ForbiddenError):
        bob.sessions.turn(
            session_b.session_id,
            query="x",
            complete=True,
            model_override={"provider": "not-allowed-provider"},
        )

    # Scope enforcement: a query token cannot ingest.
    with pytest.raises(ForbiddenError):
        bob.ingest.ingest(ingest_file("x.md", "x"))

    state["bob_token"] = bob_token


def test_removal_double_pass(
    ops: MunariumClient, mgr: MunariumClient, make_client: Callable[[str, str], MunariumClient]
) -> None:
    """Soft removal is double-pass and leaves data intact."""
    # The doomed version is NONCE'D (seconds since epoch): removal is
    # permanent, so a fixed number makes this scenario single-use against a
    # shared dev tenant (proven live by the Rust port).
    doomed_version = int(time.time()) % 2_000_000_000
    doomed = f"ent-support@{doomed_version}"
    ops.runbooks.apply_runbook(runbook_yaml(doomed_version))

    # Single-pass confirm is refused (409 removal-not-confirmed -> typed).
    with pytest.raises(InvalidInputError):
        ops.runbooks.remove_confirm(doomed, "rm-guess")

    removal = ops.runbooks.remove_request(doomed)
    assert removal.removal_id, f"removal_id missing: {removal}"

    # A WRONG removal_id must draw the SAME typed refusal as no request —
    # accepting any error here would let a transient 503 or a routing bug
    # masquerade as the double-pass guard working.
    with pytest.raises(InvalidInputError):
        ops.runbooks.remove_confirm(doomed, "rm-wrong")

    confirmed = ops.runbooks.remove_confirm(doomed, removal.removal_id)
    assert confirmed.status == "removed", f"confirm: {confirmed}"

    # Sessions on the removed exact ref: typed NotFound (410
    # runbook-removed); the bare name still resolves to a LIVE version —
    # not asserted to be @1, because earlier smoke runs against a shared
    # tenant may have left other versions, but never the one just removed.
    user = make_client(mint(mgr, "rm-user", 0, [], ["query"]).token, "rm-user")
    with pytest.raises(NotFoundError):
        user.sessions.create(doomed)
    live = user.sessions.create("ent-support")
    assert live.runbook_ref.startswith("ent-support@") and live.runbook_ref != doomed, (
        f"bare name must resolve to a live version: {live}"
    )

    # Hidden from the default list; visible with include_removed.
    assert not any(b.runbook_ref == doomed for b in ops.runbooks.list()), (
        "removed ref must be hidden from the default list"
    )
    assert any(b.runbook_ref == doomed for b in ops.runbooks.list(include_removed=True)), (
        "include_removed must show it"
    )


def test_reports_and_revoke(ops: MunariumClient, mgr: MunariumClient) -> None:
    """Reports are mgmt-gated and reflect this suite's traffic; revocation
    lands in the issuance audit."""
    with pytest.raises(ForbiddenError):
        ops.reports.usage(group_by="uid")

    usage = mgr.reports.usage(group_by="uid")
    keys = [r.key for r in usage.rows]
    assert "comp-alice" in keys and "comp-bob" in keys, (
        f"usage rows must include the session uids: {keys}"
    )

    audit = mgr.reports.audit(uid="comp-bob", limit=10)
    assert audit.entries, "audit for comp-bob must be non-empty"

    # The dashboard-view reports answer too (2026-08-18 routes).
    ts = mgr.reports.timeseries(window="24h")
    assert ts.window == "24h", f"timeseries window echo: {ts}"
    eps = mgr.reports.endpoints(window="24h", limit=5)
    assert eps.rows, "endpoint rows must reflect traffic"
    mgr.reports.runbooks(window="24h")
    sess = mgr.reports.sessions(window="24h")
    assert any(b.turns > 0 for b in sess.buckets), (
        f"sessions report must show the turns this suite took: {sess}"
    )
    mgr.reports.cost()

    # Revoke: the deny-list row lands and the audit shows it.
    jti = mint(mgr, "revokee", 0, [], ["query"]).jti
    revoked = mgr.tokens.revoke(jti)
    assert revoked.revoked, f"revoke must land: {revoked}"
    tokens = mgr.tokens.list(uid="revokee")
    row = next((t for t in tokens if t.jti == jti), None)
    assert row is not None and row.revoked_at is not None, (
        f"issuance audit must show revoked_at: {tokens}"
    )


def test_authoring_lifecycle(ops: MunariumClient) -> None:
    """Guided authoring end to end, keyless: catalog -> draft -> answers ->
    validate -> assist (degrades to a note) -> export (hash-verified
    client-side) -> apply -> hosted -> cleaned up."""
    patterns = ops.authoring.list_patterns()
    assert len(patterns) == 7, f"expected the 7 patterns, got {len(patterns)}"
    detail = ops.authoring.get_pattern("ask-the-corpus")
    assert "kind: Runbook" in detail.runbook_yaml, "pattern detail carries the exemplar"

    draft = ops.authoring.create_draft(name="vendor-security", pattern_id="ask-the-corpus")
    assert draft.draft_id, "draft_id missing"
    assert draft.interview and draft.interview[0].id == "identity", "interview starts at identity"

    # The workspace listing + readback name the draft.
    drafts = ops.authoring.list_drafts()
    assert any(d.draft_id == draft.draft_id for d in drafts), (
        "list_drafts must contain the new draft"
    )
    readback = ops.authoring.get_draft(draft.draft_id)
    assert readback.name == "vendor-security", f"draft readback: {readback}"

    # A blank draft refuses to export (409 authoring-draft-invalid -> typed).
    with pytest.raises(InvalidInputError):
        ops.authoring.export(draft.draft_id)

    answers = {
        "identity.description": "Vendor security reviews for procurement.",
        "prefix.root": "vendors/",
        "prefix.areas": [
            {"path": "public/", "description": "published attestations"},
            {"path": "contracts/", "description": "signed agreements"},
        ],
        "access.uniform_public": False,
        "access.area_levels": {"public": 0, "contracts": 2},
        "access.area_compartments": {"contracts": ["legal"]},
    }
    updated = ops.authoring.put_answers(draft.draft_id, answers)
    assert updated.validation is not None and updated.validation.valid, (
        f"canonical answers must validate clean: {updated.validation}"
    )
    assert len(updated.documents) == 2, f"one shape + one runbook: {len(updated.documents)}"

    # Assist DEGRADES keyless: 200 + assist_note, documents intact.
    assist = ops.authoring.assist(draft.draft_id)
    assert assist.assist_note is not None, "keyless assist must carry a degrade note"
    assert len(assist.documents) == 2, "assist must not lose documents"

    validation = ops.authoring.validate(draft.draft_id)
    assert validation.valid, f"validate: {validation}"

    # Export: verify the manifest CLIENT-side, exactly as mmctl does.
    bundle = ops.authoring.export(draft.draft_id)
    assert bundle.kind == "MunariumAuthoringBundle", f"bundle kind: {bundle.kind}"
    buf = ""
    for path in sorted(bundle.files):
        actual = sha(bundle.files[path])
        assert bundle.hashes.get(path) == actual, f"per-file hash mismatch for {path}"
        buf += f"{path}\0{actual}\n"
    manifest = sha(buf)
    assert bundle.manifest_hash == manifest, (
        f"manifest hash mismatch (client-recomputed {manifest})"
    )
    assert bundle.apply_order and bundle.apply_order[0].startswith("shapes/"), (
        f"shapes apply first: {bundle.apply_order}"
    )

    applied = ops.authoring.apply(draft.draft_id)
    assert len(applied.applied) == 2, "apply covers the set"
    hosted = ops.runbooks.get_info("vendor-security")
    assert len(hosted.collections) == 2, "applied runbook reaches its two collections"

    # Draft cleanup — the client surface's one DELETE (soft, workspace-only).
    deleted = ops.authoring.delete_draft(draft.draft_id)
    assert deleted.status == "deleted", f"delete: {deleted}"


def test_bulk_upload_lifecycle(
    ops: MunariumClient, mgr: MunariumClient, make_client: Callable[[str, str], MunariumClient]
) -> None:
    """Bulk upload sessions: manifest diff, chunked upload with per-file
    sha verification, replay idempotency, finalize verification, the
    zero-byte re-run — plus the CLIENT-side chunk-cap guard."""
    shape = (
        "apiVersion: munarium.ioka.io/v1\n"
        "kind: Shape\n"
        "metadata: { name: bulkdocs, version: 1 }\n"
        "spec:\n"
        "  fact:\n"
        "    schema: { type: object }\n"
    )
    ops.runbooks.apply_shape(shape)
    runbook = """apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: bulk-archive, version: 1 }
spec:
  collections:
    - name: bulk-open-docs
      shape: bulkdocs@1
      accessLevel: 0
      sources: { filenamePrefix: "bulkdocs/" }
  retrieval: { topK: 5 }
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verify: {}
    - cutover: { approval: required }
    - retireOld: { keep_versions: 2 }
"""
    ops.runbooks.apply_runbook(runbook)

    loader = make_client(mint(mgr, "bulkloader", 0, [], ["ingest"]).token, "bulkloader")
    # Nonce'd contents: this scenario re-runs against a shared dev server,
    # and the zero-byte-re-run assertion needs fresh bytes each run.
    n = nonce()
    a = f"Bulk document alpha {n}: the treaty was signed."
    b = f"Bulk document beta {n}: the harbor closed in March."
    c = f"Bulk document gamma {n}: the assembly dissolved."

    # CLIENT-side guard: an over-cap chunk never leaves the process.
    oversized = [ingest_file(f"bulkdocs/f{i}.md", "x") for i in range(501)]
    with pytest.raises(InvalidInputError, match="500"):
        loader.ingest.bulk_chunk("blk-any", oversized)

    # Manifest validation server-side: duplicates rejected whole.
    with pytest.raises(InvalidInputError):
        loader.ingest.bulk_open(
            [manifest_entry("bulkdocs/a.md", a), manifest_entry("bulkdocs/a.md", a)]
        )

    # Open: fresh manifest, all three needed.
    opened = loader.ingest.bulk_open(
        [
            manifest_entry("bulkdocs/a.md", a),
            manifest_entry("bulkdocs/b.md", b),
            manifest_entry("bulkdocs/c.md", c),
        ],
        label="py-client-conformance",
    )
    assert opened.total == 3 and opened.already_present == 0 and len(opened.needed) == 3, (
        f"fresh open must need all three: {opened}"
    )

    # Chunk 1: a good; b deliberately corrupt (per-file sha mismatch: the
    # manifest declared b's real hash, but the chunk carries other bytes).
    chunk1 = loader.ingest.bulk_chunk(
        opened.bulk_id,
        [ingest_file("bulkdocs/a.md", a), ingest_file("bulkdocs/b.md", "corrupted bytes")],
    )
    assert len(chunk1.results) == 2, f"chunk 1: expected 2 results: {chunk1}"
    assert chunk1.results[0].error is None, f"a.md must store: {chunk1}"
    assert chunk1.results[1].error is not None and "sha256 mismatch" in chunk1.results[1].error, (
        f"corrupt b.md must fail per-file: {chunk1}"
    )
    assert chunk1.stored == 1 and chunk1.failed == 1 and chunk1.pending == 1, (
        f"chunk 1 counts: {chunk1}"
    )

    # Early finalize: incomplete, naming what is owed.
    early = loader.ingest.bulk_complete(opened.bulk_id)
    assert early.status == "incomplete" and early.missing_count == 2, f"early complete: {early}"

    # Chunk 2 — wholesale replay: a again (idempotent), b fixed, c.
    chunk2 = loader.ingest.bulk_chunk(
        opened.bulk_id,
        [
            ingest_file("bulkdocs/a.md", a),
            ingest_file("bulkdocs/b.md", b),
            ingest_file("bulkdocs/c.md", c),
        ],
    )
    assert len(chunk2.results) == 3, f"chunk 2: expected 3 results: {chunk2}"
    assert chunk2.results[0].existed, f"replayed a.md must be an idempotent no-op: {chunk2}"
    assert chunk2.pending == 0 and chunk2.failed == 0, f"nothing owed after chunk 2: {chunk2}"

    # Finalize + status agree; the session's stored count survives replay.
    complete = loader.ingest.bulk_complete(opened.bulk_id)
    assert complete.status == "completed", f"complete: {complete}"
    status = loader.ingest.bulk_status(opened.bulk_id, include_needed=True)
    assert status.status == "completed" and status.needed == [] and status.stored == 3, (
        f"status after complete: {status}"
    )

    # Zero-byte re-run: same manifest, nothing owed, completes chunkless.
    rerun = loader.ingest.bulk_open(
        [
            manifest_entry("bulkdocs/a.md", a),
            manifest_entry("bulkdocs/b.md", b),
            manifest_entry("bulkdocs/c.md", c),
        ]
    )
    assert rerun.already_present == 3 and rerun.needed == [], (
        f"re-run open must owe nothing: {rerun}"
    )
    rerun_done = loader.ingest.bulk_complete(rerun.bulk_id)
    assert rerun_done.status == "completed", f"zero-byte re-run completes: {rerun_done}"

    # Unknown session: typed NotFound.
    with pytest.raises(NotFoundError):
        loader.ingest.bulk_status("blk-doesnotexist")

    # get_source is a CONTROL-plane read: static tokens only — a capability
    # JWT draws the typed 403, and the rw static token reads the metadata.
    source_id = chunk2.results[2].source_id
    assert source_id, "c.md must carry a source_id"
    with pytest.raises(ForbiddenError):
        loader.ingest.get_source(source_id)
    info = ops.ingest.get_source(source_id)
    assert info.filename == "bulkdocs/c.md" and info.content_hash == sha(c), (
        f"source metadata must match what was uploaded: {info}"
    )


def test_route_coverage(ops: MunariumClient) -> None:
    """The routes no other scenario touches: /version, the collections
    trio, chronology rules, and the findings query — so a regression in
    any of them fails a smoke instead of shipping green."""
    # GET /version (unauthenticated meta).
    version = ops.server_version()
    assert version.name == "munarium-server" and version.version, f"version handshake: {version}"

    # Collections trio (depends on entdocs@1 from the application scenario).
    name = f"cov-py-{nonce()}"
    created = ops.retrieval.create_collection(
        name=name,
        shape_ref="entdocs@1",
        access_level=1,
        compartments=["cov"],
        description="route-coverage smoke",
    )
    assert created.name == name and created.access_level == 1, f"created collection echo: {created}"
    listed = ops.retrieval.list_collections()
    assert any(col.id == created.id for col in listed), "collection must appear in the listing"
    fetched = ops.retrieval.get_collection(created.id)
    assert fetched.compartments == ["cov"] and fetched.description == "route-coverage smoke", (
        f"collection round-trip: {fetched}"
    )

    # Chronology rules: apply (upsert) + verbatim readback.
    rules_yaml = (
        "apiVersion: munarium.ioka.io/v1\n"
        "kind: ChronologyRules\n"
        "metadata: { name: cov-rules }\n"
        "spec:\n"
        "  order:\n"
        "    - { before: founding.date, after: dissolution.date }\n"
    )
    applied = ops.runbooks.apply_chronology_rules(rules_yaml)
    assert applied.name == "cov-rules" and applied.rule_count == 2, (
        f"chronology apply echo: {applied}"
    )
    readback = ops.runbooks.get_chronology_rules("cov-rules")
    assert readback == rules_yaml, "chronology rules must read back verbatim"

    # Findings query: empty on a fresh lineage, severity filter accepted,
    # and a bogus severity draws the typed rejection.
    v = ops.commands.create_version()
    findings = ops.query.findings(v, severity="block")
    assert findings == [], "fresh lineage must have no findings"
    with pytest.raises(InvalidInputError):
        ops.query.findings(v, severity="bogus")


def test_turn_stream_sse(
    state: dict[str, str], make_client: Callable[[str, str], MunariumClient]
) -> None:
    """The SSE streaming turn: progress events at real stage boundaries,
    then exactly one TurnResult (the last item); a closed session draws the
    typed session-not-open refusal — pre-stream OR as the stream's terminal
    error event (proven live on the Rust port), both decoding identically,
    and an errored stream yields nothing else."""
    bob_token = state.get("bob_token")
    assert bob_token, "application scenario did not run — no session token"
    bob = make_client(bob_token, "comp-bob")
    session = bob.sessions.create("ent-support")

    progress = 0
    done: models.TurnResult | None = None
    for item in bob.sessions.turn_stream(session.session_id, query="vacation"):
        if isinstance(item, models.TurnResult):
            assert done is None, "exactly one done event"
            done = item
        else:
            assert done is None, "no progress may arrive after the terminal done event"
            assert isinstance(item, models.TurnProgress)
            progress += 1
    assert done is not None, "stream ended without a done event"
    assert progress >= 1, f"expected at least one progress event (retrieval/merge): {progress}"
    assert done.hits and done.ordinal >= 1, f"streamed done must carry the full turn: {done}"

    closed = bob.sessions.close(session.session_id)
    assert closed.state == "closed", f"close must land: {closed}"
    # The refusal may land pre-stream (plain problem+json) or mid-stream as
    # the terminal error event — for this iterator API both raise the typed
    # InvalidInputError at call time or during iteration.
    with pytest.raises(InvalidInputError):
        for _ in bob.sessions.turn_stream(session.session_id, query="x"):
            pass


async def test_turn_stream_sse_async(state: dict[str, str]) -> None:
    """The async-REST twin of the SSE scenario: same ordering invariants
    through AsyncRestSessions.turn_stream."""
    assert REST_URL is not None
    bob_token = state.get("bob_token")
    assert bob_token, "application scenario did not run — no session token"
    client = AsyncMunariumClient.rest(ClientOptions(REST_URL, token=bob_token, uid="comp-bob"))
    try:
        session = await client.sessions.create("ent-support")
        items: list[models.TurnProgress | models.TurnResult] = []
        # aclosing is the model usage: leaving the stream early (break, an
        # exception) then releases the pooled connection deterministically
        # instead of whenever the abandoned generator is finalized.
        async with aclosing(
            client.sessions.turn_stream(session.session_id, query="vacation")
        ) as events:
            async for item in events:
                items.append(item)
        assert items and isinstance(items[-1], models.TurnResult), (
            "the LAST yielded item must be the TurnResult"
        )
        assert all(isinstance(i, models.TurnProgress) for i in items[:-1]), (
            "everything before the terminal item is progress"
        )
        assert len(items) >= 2, f"expected at least one progress event: {items}"

        closed = await client.sessions.close(session.session_id)
        assert closed.state == "closed"
        with pytest.raises(InvalidInputError):
            async with aclosing(
                client.sessions.turn_stream(session.session_id, query="x")
            ) as events:
                async for _ in events:
                    pass
    finally:
        await client.close()


def test_grpc_surface(mgr: MunariumClient) -> None:
    """gRPC halves of the platform surface: the AdminService token trio,
    the SessionService round-trip, the collections trio, and the honest
    Unsupported set — over the sync gRPC client."""
    if GRPC_URL is None:
        pytest.skip("MUNARIUM_GRPC_URL not set")
    assert MGMT_TOKEN is not None

    with MunariumClient.grpc(ClientOptions(GRPC_URL, token=MGMT_TOKEN, uid="mgr")) as gmgr:
        # Token trio over AdminService.
        minted = gmgr.tokens.mint(
            uid="py-grpc-user", access_level=2, compartments=["eng"], scopes=["query"]
        )
        listed = gmgr.tokens.list(uid="py-grpc-user", active=True)
        assert any(t.jti == minted.jti for t in listed), (
            "grpc-minted token must appear in the audit"
        )

        # Collections trio over RetrievalService (rw static token).
        with MunariumClient.grpc(ClientOptions(GRPC_URL, token=RW_TOKEN, uid="ops")) as rw:
            name = f"cov-py-grpc-{nonce()}"
            created = rw.retrieval.create_collection(name=name, shape_ref="entdocs@1")
            fetched = rw.retrieval.get_collection(created.id)
            assert fetched.name == name, "grpc collection round-trip"
            listed_cols = rw.retrieval.list_collections()
            assert any(c.id == created.id for c in listed_cols), "grpc collection listing"

        # SessionService round-trip with the minted JWT.
        with MunariumClient.grpc(
            ClientOptions(GRPC_URL, token=minted.token, uid="py-grpc-user")
        ) as user:
            session = user.sessions.create("ent-support")
            turn = user.sessions.turn(session.session_id, query="vacation")
            assert turn.hits and turn.envelopes, f"grpc turn must carry hits + envelopes: {turn}"
            readback = user.sessions.get(session.session_id)
            assert len(readback.turns) == 1, "grpc transcript readback"
            closed = user.sessions.close(session.session_id)
            assert closed.state == "closed", f"grpc close: {closed}"

            # The honest Unsupported set.
            with pytest.raises(UnsupportedError):
                user.sessions.turn_stream(session.session_id, query="x")
        with pytest.raises(UnsupportedError):
            gmgr.reports.usage()
        with pytest.raises(UnsupportedError):
            gmgr.authoring.list_patterns()

        # Revoke last so the earlier calls ran under a live token.
        revoked = gmgr.tokens.revoke(minted.jti)
        assert revoked.revoked, "grpc revoke must land"
