# SPDX-License-Identifier: Apache-2.0
"""The C5 review invariants, pinned.

Each test here corresponds to a defect the C5 review found: a silent-empty
upload on retry, a command executed twice, and a promise filter the server
drops without complaint. They are cheap and offline on purpose — the
conformance suite proves the wire, these prove the rules.
"""

from __future__ import annotations

import pytest

from munarium_client import InvalidInputError, chunks_from_bytes, chunks_from_list
from munarium_client._chunks import resolve_chunks
from munarium_client._errors import check_promise_status
from munarium_client._specs import Spec
from munarium_client.rest import _retryable


def test_bare_iterator_is_rejected_as_a_chunk_source() -> None:
    """A one-shot iterator would be exhausted by attempt 1, so the retry
    would upload ZERO bytes — which the server stores happily when no hash
    is declared. The client must refuse it up front."""
    with pytest.raises(InvalidInputError, match="REPLAYABLE"):
        resolve_chunks(iter([b"a", b"b"]))  # type: ignore[arg-type]


@pytest.mark.parametrize(
    "factory", [chunks_from_bytes(b"abcdef"), chunks_from_list([b"ab", b"cd"])]
)
def test_chunk_factories_replay_identically(factory: object) -> None:
    assert callable(factory)
    first = b"".join(factory())
    second = b"".join(factory())
    assert first == second and first, "a source must yield the same bytes every attempt"


def test_bytes_pass_through_and_replay() -> None:
    assert resolve_chunks(b"payload") == b"payload"


def _spec(retry: str) -> Spec[None]:
    return Spec("POST", "/v1/x", parse=lambda _: None, retry=retry)  # type: ignore[arg-type]


def test_command_is_not_retried_once_it_may_have_been_delivered() -> None:
    """The server records an idempotency key only AFTER the command
    completes, so a retry overtaking an in-flight attempt executes twice."""
    assert not _retryable(_spec("command"), attempt=1, retries=2, delivered=True)


def test_command_is_retried_when_it_provably_never_left() -> None:
    assert _retryable(_spec("command"), attempt=1, retries=2, delivered=False)


def test_read_is_retried_either_way() -> None:
    assert _retryable(_spec("read"), attempt=1, retries=2, delivered=True)
    assert _retryable(_spec("read"), attempt=1, retries=2, delivered=False)


def test_write_never_retries() -> None:
    assert not _retryable(_spec("write"), attempt=1, retries=2, delivered=False)


def test_retry_budget_is_still_bounded() -> None:
    assert not _retryable(_spec("read"), attempt=3, retries=2, delivered=False)


def test_unknown_promise_status_is_rejected_not_silently_dropped() -> None:
    """The server FILTERS an unrecognized status and returns 200 with an
    empty list — a silent wrong answer about outstanding obligations."""
    with pytest.raises(InvalidInputError, match="unknown promise status"):
        check_promise_status("Open")


@pytest.mark.parametrize("status", ["open", "fulfilled", "expired", "violated", None])
def test_known_promise_statuses_pass(status: str | None) -> None:
    check_promise_status(status)
