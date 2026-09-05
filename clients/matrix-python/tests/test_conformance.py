# SPDX-License-Identifier: Apache-2.0
"""Conformance for the Python Matrix client.

Two tiers, deliberately:

* **Offline** — the response SHAPES this client claims to understand, driven
  through a stub transport. These run everywhere and are what catch a field
  rename in the API.
* **Live** — against a real Matrix when `MUNARIUM_MATRIX_TEST_URL` is set,
  skipped OUT LOUD otherwise. A skip that prints nothing is
  indistinguishable from a pass, which is how a tier stays vacuously green
  for a phase.

There is no mock of Matrix's *semantics* here. A client test that asserted
what a refusal means would be asserting its own opinion; these assert only
that the client reads what the service says.
"""

from __future__ import annotations

import os

import httpx
import pytest

from munarium_matrix import MatrixClient, MatrixError


def client_over(handler) -> MatrixClient:
    """A client whose transport is a function, so a test states the exact
    bytes the service would have sent."""
    mx = MatrixClient("http://matrix.test", token="t")
    mx._http = httpx.Client(
        transport=httpx.MockTransport(handler),
        headers=mx._headers,
    )
    return mx


def test_version_reports_lockstep_from_the_services_own_word():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == "/version"
        return httpx.Response(
            200,
            json={
                "version": "0.1.0",
                "contract_version": "0.1.0",
                "role": "all",
                "server_version": "0.5.0",
                "target_server_version": "0.5.0",
                "server_compatibility": "exact",
            },
        )

    v = client_over(handler).version()
    assert v.lockstep_ok
    assert v.role == "all"


def test_a_non_exact_lockstep_is_not_ok():
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            json={
                "version": "0.1.0",
                "contract_version": "0.1.0",
                "role": "all",
                "server_compatibility": "minor_behind",
            },
        )

    # The distinction the whole lockstep exists for: an id minted against a
    # server that does not agree on the contract may not resolve there.
    assert not client_over(handler).version().lockstep_ok


def test_apply_posts_yaml_as_yaml_and_reports_unchanged():
    def handler(request: httpx.Request) -> httpx.Response:
        assert request.headers["content-type"] == "text/yaml"
        assert b"kind: DataSource" in request.content
        return httpx.Response(
            200, json={"asset_ref": "crm@2", "kind": "DataSource", "unchanged": True}
        )

    outcome = client_over(handler).apply("kind: DataSource\n")
    assert outcome.asset_ref == "crm@2"
    # Re-applying identical bytes is ordinary GitOps, not an error.
    assert outcome.unchanged


def test_a_refusal_surfaces_its_class_and_code_rather_than_prose():
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            429,
            json={
                "type": "https://munarium.ioka.io/problems/matrix/budget",
                "title": "exhausted",
                "status": 429,
                "detail": "source 'crm' has 0 of 2 unit(s) left this hour",
                "refusal": {"class": "exhausted", "code": "budget_exceeded"},
            },
        )

    with pytest.raises(MatrixError) as caught:
        client_over(handler).verify("open-pipeline-by-region")
    err = caught.value
    assert err.code == "budget_exceeded"
    assert err.refusal_class == "exhausted"
    # A caller deciding whether to retry must not be parsing prose to do it.
    assert err.retryable


def test_a_denial_is_not_retryable():
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            403,
            json={
                "title": "forbidden",
                "status": 403,
                "detail": "role 'ro' cannot execute commands",
                "refusal": {"class": "denied", "code": "policy_denied"},
            },
        )

    with pytest.raises(MatrixError) as caught:
        client_over(handler).sync("crm")
    # Repeating a request against a door locked on purpose is not a retry.
    assert not caught.value.retryable


def test_verify_reports_which_question_moved():
    def handler(_: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            json={
                "contract": "open-pipeline-by-region@3",
                "passed": 0,
                "failed": 1,
                "questions": [
                    {
                        "question": "What is the open pipeline by region?",
                        "ok": False,
                        "rows": 1,
                        "failures": ["expected 3 rows, got 1"],
                    }
                ],
            },
        )

    out = client_over(handler).verify("open-pipeline-by-region")
    # The call succeeded and the CONTRACT did not: different things.
    assert out.failed == 1
    assert out.questions[0].failures == ["expected 3 rows, got 1"]


def test_verify_view_falls_back_from_metric_view_to_data_view():
    seen: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        seen.append(request.url.path)
        if "metricviews" in request.url.path:
            return httpx.Response(404, json={"title": "not found", "status": 404})
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


def test_a_transport_failure_is_unavailable_not_a_bare_exception():
    def handler(_: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError("connection refused")

    with pytest.raises(MatrixError) as caught:
        client_over(handler).healthdata()
    assert caught.value.refusal_class == "unavailable"
    assert caught.value.retryable


def test_no_method_on_this_client_seals_evidence():
    # The design decision, asserted rather than described: an SDK that could
    # seal would invite an application to assert provenance it cannot vouch
    # for. Evidence is READ through the server's client.
    surface = {n for n in dir(MatrixClient) if not n.startswith("_")}
    assert not {n for n in surface if "seal" in n}
    assert not {n for n in surface if "evidence" in n}


@pytest.mark.skipif(
    not os.environ.get("MUNARIUM_MATRIX_TEST_URL"),
    reason="SKIPPED OUT LOUD: set MUNARIUM_MATRIX_TEST_URL to run against a real Matrix",
)
def test_live_version_and_registry_round_trip():
    mx = MatrixClient(
        os.environ["MUNARIUM_MATRIX_TEST_URL"],
        token=os.environ.get("MUNARIUM_MATRIX_TEST_TOKEN"),
    )
    v = mx.version()
    assert v.version
    assert v.contract_version
    # The registry answers, and a listing is a list even when empty.
    assert isinstance(mx.list_assets("datasources"), list)
