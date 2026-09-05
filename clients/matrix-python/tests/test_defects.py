# SPDX-License-Identifier: Apache-2.0
"""Regression tests for the six defects found on 2026-08-30.

The first Python client shipped with these, and the offline conformance suite
was green the whole time — because it asserted the client's own behaviour
rather than the service's. Two independent readings of `rest.rs` and `dto.rs`,
done while porting this surface to .NET and to Java, found the same six. Each
one is pinned here against the bytes the service actually sends.

The lesson worth keeping is in the shape of the list: five of the six are
**silent wrong answers**, not crashes. A gate that reads "never measured", a
listing that is quietly latest-only, a fallback that never fires — these look
like a working client reporting an uneventful system.
"""

from __future__ import annotations

import httpx
import pytest

from munarium_matrix import MatrixClient, MatrixError


def client_over(handler) -> MatrixClient:
    mx = MatrixClient("http://matrix.test", token="t")
    mx._http = httpx.Client(
        transport=httpx.MockTransport(handler),
        headers=mx._headers,
    )
    return mx


# --- 1: the gate numbers live inside `gates` --------------------------------


def test_promotion_gate_numbers_are_read_from_the_gates_object():
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            json={
                "mapping": "captable-holdings@3",
                "mode": "authoritative",
                "promoted": True,
                "promoted_version": 3,
                "decision_id": "DEC-17",
                "authority_scopes": 2,
                "gates": {
                    "identity_precision": 1.0,
                    "value_conformance": 1.0,
                    "min_identity_precision": 0.95,
                    "min_value_conformance": 0.99,
                },
            },
        )

    status = client_over(handler).promotion_status("captable-holdings")
    assert status.gates is not None
    assert status.gates.pass_
    # Read at the top level these were None on every real response, which
    # reads as "this mapping has never been measured" — the calmer-sounding
    # wrong answer, and so the worse one.
    assert status.identity_precision == 1.0
    assert status.value_conformance == 1.0
    # The service answers `name@version`; the caller's bare name would lose
    # which version the status is about.
    assert status.mapping == "captable-holdings@3"
    assert status.decision_id == "DEC-17"


def test_a_mapping_with_no_completed_run_has_no_gates_rather_than_zeroes():
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200, json={"mapping": "m@1", "mode": "shadow", "promoted": False}
        )

    status = client_over(handler).promotion_status("m")
    # `None`, not 0.0: "never measured" and "measured at zero" are different
    # facts and must not render alike.
    assert status.gates is None
    assert status.identity_precision is None


# --- 2: the listing flag the service actually reads -------------------------


def test_all_versions_sends_the_parameter_the_service_deserializes():
    seen: dict[str, str] = {}

    def handler(request: httpx.Request) -> httpx.Response:
        seen.update(dict(request.url.params))
        return httpx.Response(200, json={"assets": []})

    client_over(handler).list_assets("datasources", all_versions=True)
    # `ListQuery` deserializes `all_versions`. Sending `all` was dropped in
    # silence, so the listing was ALWAYS latest-only — indistinguishable from
    # a registry with no history.
    assert seen == {"all_versions": "true"}


# --- 3: `refusal` is not always an object -----------------------------------


def test_an_asset_validation_422_raises_a_matrix_error_not_an_attribute_error():
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            422,
            json={
                "type": "https://munarium.ioka.io/problems/matrix/asset-invalid",
                "title": "asset failed validation",
                "status": 422,
                "detail": "2 error finding(s); nothing was applied",
                # An ARRAY under the same key the refusal object uses.
                "refusal": [
                    {
                        "code": "source.host-missing",
                        "path": "spec",
                        "message": "no host",
                    },
                    {
                        "code": "result.no-key",
                        "path": "spec.result",
                        "message": "unkeyed",
                    },
                ],
            },
        )

    with pytest.raises(MatrixError) as caught:
        client_over(handler).apply("kind: DataSource\n")
    err = caught.value
    assert err.status == 422
    # No class or code to mine out of a list, and that is fine — what matters
    # is that the most ordinary failure Matrix produces does not blow up
    # inside the error path itself.
    assert err.code is None
    assert "nothing was applied" in str(err)


# --- 4: the service's own validity verdict ----------------------------------


def test_validate_reports_the_services_verdict_not_the_length_of_the_findings():
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            json={
                "valid": True,
                "findings": [
                    {
                        "code": "mapping.authority-inert",
                        "path": "spec.authority",
                        "message": "declared and unreachable in shadow mode",
                    }
                ],
            },
        )

    outcome = client_over(handler).validate("kind: ClaimMapping\n")
    # Advisory findings do not block. "An empty list means valid" would refuse
    # three healthy assets — a client disagreeing with the service that
    # enforces the rules, which is the drift this package exists to avoid.
    assert outcome.valid
    assert len(outcome.findings) == 1


# --- 5: the data-view fallback was dead code --------------------------------


def test_verify_view_falls_back_on_the_422_a_missing_metric_view_really_produces():
    seen: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request.url.path)
        if "metricviews" in request.url.path:
            # What the service ACTUALLY sends: the runtime turns a registry
            # miss into `Refusal::not_covered`, which is 422 — never 404.
            return httpx.Response(
                422,
                json={
                    "title": "not covered",
                    "status": 422,
                    "detail": "no MetricView named 'pipeline-by-region' is registered",
                    "refusal": {"class": "not_covered", "code": "not_covered"},
                },
            )
        return httpx.Response(
            200,
            json={
                "contract": "pipeline-by-region@2",
                "passed": 1,
                "failed": 0,
                "fingerprint": "sha256:abc",
                "questions": [],
            },
        )

    out = client_over(handler).verify_view("pipeline-by-region")
    assert out.fingerprint == "sha256:abc"
    assert seen == [
        "/v1/metricviews/pipeline-by-region/verify",
        "/v1/dataviews/pipeline-by-region/verify",
    ]


def test_a_422_that_is_not_not_covered_is_not_swallowed_by_the_fallback():
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            422,
            json={
                "title": "metric view changed",
                "status": 422,
                "detail": "the definition moved since it was verified",
                "refusal": {"class": "not_covered", "code": "metric_view_changed"},
            },
        )

    # A changed definition is a real answer about a metric view that EXISTS.
    # Retrying it as a data view would turn a precise refusal into "no such
    # view", which is a worse error about a different thing.
    with pytest.raises(MatrixError) as caught:
        client_over(handler).verify_view("pipeline-by-region")
    assert caught.value.code == "metric_view_changed"


# --- 6: the async twin is the same surface ----------------------------------


def test_the_async_twin_offers_every_method_the_sync_one_does():
    from munarium_matrix import AsyncMatrixClient

    def surface(cls: type) -> set[str]:
        return {
            n for n in dir(cls) if not n.startswith("_") and callable(getattr(cls, n))
        }

    sync = surface(MatrixClient) - {"close"}
    async_ = surface(AsyncMatrixClient) - {"aclose"}
    # The class docstring promises this. It omitted five methods —
    # healthdata, gate_history, promote, demote, rollback — which is exactly
    # the trap for a caller porting between them that the docstring names.
    assert sync == async_, (
        f"only on sync: {sync - async_}; only on async: {async_ - sync}"
    )


# --- retry_after: the service says WHEN --------------------------------------


def test_an_exhausted_refusal_carries_the_wait_the_service_stated():
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            429,
            json={
                "title": "exhausted",
                "status": 429,
                "detail": "source 'crm' has 0 of 2 unit(s) left this hour",
                "refusal": {
                    "class": "exhausted",
                    "code": "budget_exceeded",
                    "retry_after_seconds": 1800,
                },
            },
        )

    with pytest.raises(MatrixError) as caught:
        client_over(handler).verify("open-pipeline-by-region")
    # A caller pacing a retry should not have to guess when the service
    # already said.
    assert caught.value.retry_after == 1800
    assert caught.value.retryable


def test_no_dead_helper_survives_in_the_client_module():
    # `_unused` claimed to keep `json` imported "for callers that pass
    # pre-serialised bodies". Neither half was true, and a comment that reads
    # as load-bearing over code that does nothing is worse than the dead code.
    import munarium_matrix._client as m

    assert not hasattr(m, "_unused")
    assert not hasattr(m, "_json")
