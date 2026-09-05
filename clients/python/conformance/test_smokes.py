# SPDX-License-Identifier: Apache-2.0
"""Per-plane smokes beyond the 7 core scenarios (postgres store required):
streamed content-addressed ingest, retrieval with the ProvenanceEnvelope,
shape publication, the runbook approve flow, the BYOK provider surface, and
the documented gRPC transport gaps (honest Unsupported, never silent)."""

from __future__ import annotations

import hashlib
import time

import pytest

from munarium_client import MunariumClient, ProviderError, UnsupportedError, chunks_from_list

SHAPE_YAML = """
apiVersion: munarium.ioka.io/v1
kind: Shape
metadata: { name: pysmoke-tickets, version: 1 }
spec:
  fact:
    schema:
      type: object
      properties:
        subject: { type: string }
        key: { type: string }
        value: { type: string, minLength: 1 }
      required: [subject, key, value]
    supersession:
      identity: [subject, key]
  chunking: { max_chars: 400 }
  indexing: { rrf_k: 60 }
"""

RUNBOOK_YAML = """
apiVersion: munarium.ioka.io/v1
kind: Runbook
metadata: { name: pysmoke-reindex, version: 1 }
spec:
  shape: pysmoke-tickets@1
  steps:
    - resolveSources: {}
    - buildIndex: {}
    - verify: {}
    - cutover: { approval: required }
    - retireOld: { keep_versions: 2 }
"""

PROVIDER_YAML = """
apiVersion: munarium.ioka.io/v1
kind: ProviderConfig
metadata: { name: pysmoke-anthropic }
spec:
  provider: anthropic
  models: { complete: [claude-sonnet-4-6] }
  credentialRef: { env: MUNARIUM_SECRET_PYSMOKE_UNSET }
"""

SHAPE_REF = "pysmoke-tickets@1"


def _two_chunks(data: bytes) -> list[bytes]:
    mid = len(data) // 2
    return [data[:mid], data[mid:]]


def test_plane_smokes(rest_client: MunariumClient, grpc_client: MunariumClient) -> None:
    rest = rest_client
    version = rest.commands.create_version()

    # shapes: publication witnessed in the named lineage
    applied = rest.runbooks.apply_shape(SHAPE_YAML, version_id=version)
    assert applied.shape_ref == SHAPE_REF
    assert applied.event_id, "publication must record a ledger event"

    # ingest: streamed, content-addressed, idempotent by content
    content = (
        f"alpha ticket {time.time_ns():x}: the printer prints. "
        f"beta ticket: the printer does not print."
    ).encode()
    declared = hashlib.sha256(content).hexdigest()
    first = rest.ingest.put_source(
        chunks_from_list(_two_chunks(content)),
        declared_sha256=declared,
        media_type="text/plain",
        filename="pysmoke.txt",
        shape_ref=SHAPE_REF,
    )
    assert first.content_hash == declared and not first.already_existed
    second = rest.ingest.put_source(
        chunks_from_list(_two_chunks(content)),
        declared_sha256=declared,
        filename="pysmoke.txt",
        shape_ref=SHAPE_REF,
    )
    assert second.already_existed, "re-upload of the same path + bytes must be already_existed"

    recorded = rest.ingest.record_ingest(version, content_hash=declared, shape_ref=SHAPE_REF)
    assert recorded.event_id and recorded.seq > 0

    # retrieval: build + search; the envelope is load-bearing
    built = rest.retrieval.build_index(SHAPE_REF, version_id=version)
    assert built.active and built.index_version
    result = rest.retrieval.search(query="printer", shape_ref=SHAPE_REF, top_k=5)
    assert result.hits, "expected lexical hits for a word in the source"
    assert result.envelope.index_version, "every answer carries a ProvenanceEnvelope"

    # runbooks: run -> awaiting_approval -> approve -> done
    rest.runbooks.apply_runbook(RUNBOOK_YAML)
    run = rest.runbooks.run_runbook("pysmoke-reindex", version_id=version)
    assert run.state == "awaiting_approval", f"run must pause at the gate: {run.state}"
    status = rest.runbooks.get_run(run.run_id)
    awaiting = next(s for s in status.steps if s.state == "awaiting_approval")
    approved = rest.runbooks.approve_step(run.run_id, awaiting.ordinal)
    assert approved.state == "done", f"approved run must complete: {approved.state}"

    # providers: BYOK surface answers (fail-closed creds — no live LLM call)
    assert rest.providers.apply_config(PROVIDER_YAML) == "pysmoke-anthropic"
    try:
        health = rest.providers.health("pysmoke-anthropic")
        assert isinstance(health.healthy, bool)
    except ProviderError:
        pass  # typed fail-closed credential error is the expected demo outcome

    # -- gRPC cross-plane half ---------------------------------------------
    grpc = grpc_client
    assert grpc.runbooks.apply_shape(SHAPE_YAML).shape_ref == SHAPE_REF

    content2 = f"gamma ticket {time.time_ns():x}: the scanner scans.".encode()
    declared2 = hashlib.sha256(content2).hexdigest()
    uploaded = grpc.ingest.put_source(
        chunks_from_list(_two_chunks(content2)),
        declared_sha256=declared2,
        media_type="text/plain",
        filename="pysmoke-grpc.txt",
        shape_ref=SHAPE_REF,
    )
    assert uploaded.content_hash == declared2
    assert grpc.ingest.record_ingest(version, content_hash=declared2).seq > 0

    result = grpc.retrieval.search(query="printer", shape_ref=SHAPE_REF, top_k=5)
    assert result.hits and result.envelope.index_version

    with pytest.raises(UnsupportedError):
        grpc.retrieval.build_index(SHAPE_REF)

    # cross-plane run visibility — version_id now rides the wire (C5)
    grpc_run = grpc.runbooks.get_run(run.run_id)
    assert grpc_run.state == "done"
    assert grpc_run.version_id == version

    # invocation provenance is accepted on gRPC (C5): the fail-closed
    # credential error proves the call reached the provider gateway rather
    # than being rejected as an unsupported transport.
    with pytest.raises(ProviderError):
        grpc.providers.complete("pysmoke-anthropic", prompt="hi", version_id=version)
