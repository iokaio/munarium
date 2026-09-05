# SPDX-License-Identifier: Apache-2.0
"""Offline unit tests for the C8 surface: the new slug decodes (run-locked
non-transience, the sessions/authoring 409s), the client-side bulk cap
guard, and the gRPC file plane's per-item base64 splice contract."""

from __future__ import annotations

import base64
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "src"))

import pytest  # noqa: E402

from munarium_client import InvalidInputError, RunLockedError  # noqa: E402
from munarium_client._errors import (  # noqa: E402
    BULK_MAX_FILES_PER_CHUNK,
    check_bulk_files,
    error_from_grpc,
    error_from_problem,
)
from munarium_client._grpc_common import (  # noqa: E402
    parse_ingest_result,
    prepare_ingest_files,
    splice_ingest_results,
)
from munarium_client.models import IngestFile, IngestResult  # noqa: E402


def problem(slug: str, status: int) -> dict[str, object]:
    return {
        "type": f"https://munarium.ioka.io/problems/{slug}",
        "title": slug,
        "status": status,
        "detail": f"{slug} detail",
    }


# ---------------------------------------------------------------------------
# slugs
# ---------------------------------------------------------------------------


def test_run_locked_is_typed_and_never_transient() -> None:
    # Before this slug was mapped it decoded as Unexpected — hiding that
    # the request was rejected pre-execution and a later re-run succeeds
    # once the lock clears. A run lock is held for a whole run (minutes),
    # so it is deliberately NOT transient: pace yourself, like RateLimited.
    err = error_from_problem(409, problem("run-locked", 409))
    assert isinstance(err, RunLockedError)
    assert err.slug == "run-locked"
    assert not err.transient, "auto-retry with sub-second jitter would be futile churn"


def test_run_locked_decodes_on_grpc_too() -> None:
    from google.rpc import error_details_pb2

    info = error_details_pb2.ErrorInfo(reason="run-locked", domain="mmp.ioka.io")
    err = error_from_grpc("ABORTED", "run run-1 holds the lock", info)
    assert isinstance(err, RunLockedError) and not err.transient


@pytest.mark.parametrize("slug", ["session-not-open", "authoring-draft-invalid"])
def test_lifecycle_409s_map_to_invalid_input(slug: str) -> None:
    # The removal-not-confirmed precedent: same status-class convention.
    err = error_from_problem(409, problem(slug, 409))
    assert isinstance(err, InvalidInputError)


# ---------------------------------------------------------------------------
# bulk cap guard
# ---------------------------------------------------------------------------


def test_bulk_cap_guard_names_the_surface_and_the_cap() -> None:
    with pytest.raises(InvalidInputError, match=r"bulk chunk .*500.*\(got 501\)"):
        check_bulk_files("bulk chunk", BULK_MAX_FILES_PER_CHUNK + 1)
    with pytest.raises(InvalidInputError, match=r"batch .*\(got 0\)"):
        check_bulk_files("batch", 0)
    check_bulk_files("batch", 1)
    check_bulk_files("bulk chunk", BULK_MAX_FILES_PER_CHUNK)


# ---------------------------------------------------------------------------
# gRPC per-item base64 splice (pure functions — no channel needed)
# ---------------------------------------------------------------------------


def _file(name: str, content_b64: str) -> IngestFile:
    return IngestFile(filename=name, media_type="text/plain", content_base64=content_b64)


def test_bad_base64_becomes_its_own_error_result_and_the_rest_splice_in_order() -> None:
    # The per-item contract holds ACROSS transports: a file whose base64
    # cannot decode becomes its own error result (never sent), the valid
    # remainder ships, and results splice back in input order.
    good = base64.b64encode(b"hello").decode()
    files = [_file("a.md", good), _file("b.md", "%%not-base64%%"), _file("c.md", good)]
    slots = prepare_ingest_files(files)
    assert isinstance(slots[1], IngestResult)
    assert slots[1].error is not None and "base64" in slots[1].error
    to_send = [s for s in slots if not isinstance(s, IngestResult)]
    assert [f.filename for f in to_send] == ["a.md", "c.md"]
    assert to_send[0].content == b"hello", "decoded to native bytes for the wire"

    server = [
        IngestResult(filename="a.md", source_id="src-a", existed=False, bound_to=["docs"]),
        IngestResult(filename="c.md", source_id="src-c", existed=True, bound_to=[]),
    ]
    merged = splice_ingest_results(slots, server)
    assert [r.filename for r in merged] == ["a.md", "b.md", "c.md"]
    assert merged[0].source_id == "src-a"
    assert merged[1].error is not None and merged[1].source_id is None
    assert merged[2].existed is True


def test_a_short_server_result_list_is_an_error_not_a_silent_success() -> None:
    from munarium_client import UnexpectedError

    good = base64.b64encode(b"x").decode()
    slots = prepare_ingest_files([_file("a.md", good), _file("b.md", good)])
    only_one = [IngestResult(filename="a.md", existed=False, bound_to=[])]
    with pytest.raises(UnexpectedError, match="b.md"):
        splice_ingest_results(slots, only_one)


def test_parse_ingest_result_maps_proto3_empties_to_none() -> None:
    from munarium_client._proto.mmp.v1 import ingest_pb2

    pb = ingest_pb2.IngestResult(
        filename="a.md", source_id="", sha256="", existed=False, bound_to=[], error=""
    )
    r = parse_ingest_result(pb)
    assert r.source_id is None and r.sha256 is None and r.error is None
