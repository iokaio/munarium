# SPDX-License-Identifier: Apache-2.0
"""The REST client. Sync and async, one shared request shape."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any

import httpx

if TYPE_CHECKING:
    # `Self` is `typing.Self` only from 3.11, and this package's floor is
    # 3.10. `typing_extensions` is not one of its runtime dependencies: type
    # checkers carry it, and with postponed annotations the name is never
    # evaluated at runtime, so it is imported for them alone.
    from typing_extensions import Self

DEFAULT_TIMEOUT = 30.0
"""Seconds. Long enough for a verify that runs a contract's whole suite
against a cold warehouse, short enough that a wedged call is not forever."""


class MatrixError(Exception):
    """A refusal, or a transport failure.

    Matrix answers a refusal as RFC 9457 problem+json with a `refusal` object
    carrying the CLASS and the CODE — the closed vocabulary the whole system
    is built on. Those are surfaced as attributes rather than flattened into
    the message, because a caller that must distinguish "not covered" from
    "budget exhausted" should not be parsing prose to do it.
    """

    def __init__(
        self,
        message: str,
        *,
        status: int | None = None,
        code: str | None = None,
        refusal_class: str | None = None,
        detail: str | None = None,
        retry_after: int | None = None,
    ) -> None:
        super().__init__(message)
        self.status = status
        self.code = code
        self.refusal_class = refusal_class
        self.detail = detail
        #: Seconds the SERVICE said to wait, when it said so. `None` is "it
        #: did not say", never "retry immediately".
        self.retry_after = retry_after

    @property
    def retryable(self) -> bool:
        """Whether retrying the SAME request could plausibly succeed.

        `unavailable` and `exhausted` are states of the world; the rest are
        statements about the request or the assets, and repeating it changes
        nothing. A caller that retries a `denied` is hammering a door that is
        locked on purpose.
        """
        return self.refusal_class in ("unavailable", "exhausted")


@dataclass(frozen=True)
class Version:
    version: str
    contract_version: str
    role: str
    server_version: str | None = None
    target_server_version: str | None = None
    server_compatibility: str | None = None
    uptime_seconds: int | None = None

    @property
    def lockstep_ok(self) -> bool:
        """Matrix and the server it seals into must agree on the contract.

        `exact` is the only state in which an evidence id minted here is
        certain to resolve there.
        """
        return self.server_compatibility == "exact"


@dataclass(frozen=True)
class ApplyOutcome:
    asset_ref: str
    kind: str
    unchanged: bool = False


@dataclass(frozen=True)
class ValidationFinding:
    code: str
    path: str
    message: str


@dataclass(frozen=True)
class VerifiedQuestion:
    question: str
    ok: bool
    rows: int | None = None
    logical_result_hash: str | None = None
    failures: list[str] = field(default_factory=list)


@dataclass(frozen=True)
class VerifyOutcome:
    contract: str
    passed: int
    failed: int
    questions: list[VerifiedQuestion] = field(default_factory=list)
    #: Semantic views only: the definition the questions ran under.
    fingerprint: str | None = None


@dataclass(frozen=True)
class JobAccepted:
    accepted: int
    jobs: list[str]
    detail: str = ""


@dataclass(frozen=True)
class PromotionGates:
    """The two numbers a promotion is decided on, and the minimums they were
    measured against — all four together, because a precision of 0.97 means
    nothing until you know whether the bar was 0.95 or 0.99."""

    identity_precision: float
    value_conformance: float
    min_identity_precision: float
    min_value_conformance: float

    @property
    def pass_(self) -> bool:
        return (
            self.identity_precision >= self.min_identity_precision
            and self.value_conformance >= self.min_value_conformance
        )


@dataclass(frozen=True)
class PromotionStatus:
    mapping: str
    promoted: bool
    mode: str = ""
    promoted_version: int | None = None
    decision_id: str | None = None
    promoted_at: str | None = None
    authority_scopes: int = 0
    #: `None` means no completed run has been measured — which is a different
    #: thing from a run that measured badly, and the two must not read alike.
    gates: PromotionGates | None = None
    latest_run: dict[str, Any] | None = None

    @property
    def identity_precision(self) -> float | None:
        return self.gates.identity_precision if self.gates else None

    @property
    def value_conformance(self) -> float | None:
        return self.gates.value_conformance if self.gates else None


def _promotion(raw: Mapping[str, Any], fallback_name: str) -> PromotionStatus:
    """Decode a promotion status.

    The gate numbers live inside `gates`, NOT at the top level. Reading them
    at the top level — which this client did until 2026-08-30 — returns
    `None` for every mapping that has ever run, and "never measured" is the
    calmer-sounding wrong answer, which is what made it worth a test.
    """
    g = raw.get("gates")
    gates = (
        PromotionGates(
            identity_precision=float(g.get("identity_precision", 0.0)),
            value_conformance=float(g.get("value_conformance", 0.0)),
            min_identity_precision=float(g.get("min_identity_precision", 0.0)),
            min_value_conformance=float(g.get("min_value_conformance", 0.0)),
        )
        if isinstance(g, Mapping)
        else None
    )
    return PromotionStatus(
        # The service answers `name@version`; keeping the caller's bare name
        # would discard which VERSION the status is about.
        mapping=str(raw.get("mapping") or fallback_name),
        promoted=bool(raw.get("promoted", False)),
        mode=str(raw.get("mode", "")),
        promoted_version=raw.get("promoted_version"),
        decision_id=raw.get("decision_id"),
        promoted_at=raw.get("promoted_at"),
        authority_scopes=int(raw.get("authority_scopes", 0)),
        gates=gates,
        latest_run=raw.get("latest_run"),
    )


@dataclass(frozen=True)
class Validation:
    """What `validate` answers.

    `valid` comes from the SERVICE. Three finding codes are advisory —
    `limits.above-inline-seal`, `mapping.authority-inert`,
    `authorization.classes-ignored` — so an asset that is valid and will
    apply can still carry findings, and a client that treated "findings is
    non-empty" as invalid would refuse three healthy assets. That is exactly
    the local-validation drift this package refuses to introduce.
    """

    valid: bool
    findings: list[ValidationFinding] = field(default_factory=list)


def _finding(raw: Mapping[str, Any]) -> ValidationFinding:
    return ValidationFinding(
        code=str(raw.get("code", "")),
        path=str(raw.get("path", "")),
        message=str(raw.get("message", "")),
    )


def _verify(raw: Mapping[str, Any]) -> VerifyOutcome:
    return VerifyOutcome(
        contract=str(raw.get("contract", "")),
        passed=int(raw.get("passed", 0)),
        failed=int(raw.get("failed", 0)),
        fingerprint=raw.get("fingerprint"),
        questions=[
            VerifiedQuestion(
                question=str(q.get("question", "")),
                ok=bool(q.get("ok", False)),
                rows=q.get("rows"),
                logical_result_hash=q.get("logical_result_hash"),
                failures=list(q.get("failures", [])),
            )
            for q in raw.get("questions", [])
        ],
    )


def _looks_like_no_such_view(exc: MatrixError) -> bool:
    """Is this "there is no metric view by that name", so a data view is worth
    trying?

    Not `status == 404`. `/v1/metricviews/{name}/verify` loads through the
    runtime, which turns a registry miss into `Refusal::not_covered` — HTTP
    **422**, not 404. Keying on 404 alone made the data-view fallback dead
    code: `verify_view` on a native data view raised "no MetricView named X
    is registered" and never tried the other route.
    """
    if exc.status == 404:
        return True
    return exc.status == 422 and exc.code == "not_covered"


def _raise_for(response: httpx.Response) -> None:
    if response.is_success:
        return
    try:
        body = response.json()
    except ValueError:
        body = {}
    # `refusal` is not always a refusal object: an asset-validation 422 puts
    # the findings ARRAY under the same key. Reading `.get` off a list is an
    # AttributeError raised from inside the error path, which is how the most
    # ordinary failure Matrix produces became the one that crashed hardest.
    raw_refusal = body.get("refusal")
    refusal: Mapping[str, Any] = raw_refusal if isinstance(raw_refusal, Mapping) else {}
    raise MatrixError(
        body.get("detail")
        or body.get("title")
        or f"matrix answered {response.status_code}",
        status=response.status_code,
        code=refusal.get("code"),
        refusal_class=refusal.get("class"),
        detail=body.get("detail"),
        # The service says WHEN to come back. A caller pacing an `exhausted`
        # refusal should not have to guess.
        retry_after=refusal.get("retry_after_seconds"),
    )


class _Base:
    def __init__(
        self,
        base_url: str,
        *,
        token: str | None = None,
        uid: str | None = None,
        timeout: float = DEFAULT_TIMEOUT,
    ) -> None:
        self._base = base_url.rstrip("/")
        self._timeout = timeout
        headers: dict[str, str] = {}
        if token:
            headers["authorization"] = f"Bearer {token}"
        if uid:
            # The server-side planes require a uid on every /v1 request;
            # Matrix does not, but sending it keeps one identity across both
            # journals when the same operator drives them.
            headers["x-munarium-uid"] = uid
        self._headers = headers

    def _url(self, path: str) -> str:
        return f"{self._base}{path}"


class MatrixClient(_Base):
    """Synchronous client."""

    def __init__(self, base_url: str, **kwargs: Any) -> None:
        super().__init__(base_url, **kwargs)
        self._http = httpx.Client(timeout=self._timeout, headers=self._headers)

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def close(self) -> None:
        self._http.close()

    def _request(self, method: str, path: str, **kwargs: Any) -> httpx.Response:
        try:
            response = self._http.request(method, self._url(path), **kwargs)
        except httpx.HTTPError as exc:  # transport, not refusal
            raise MatrixError(str(exc), refusal_class="unavailable") from exc
        _raise_for(response)
        return response

    # -- meta ---------------------------------------------------------------

    def version(self) -> Version:
        raw = self._request("GET", "/version").json()
        return Version(
            version=raw.get("version", ""),
            contract_version=raw.get("contract_version", ""),
            role=raw.get("role", ""),
            server_version=raw.get("server_version"),
            target_server_version=raw.get("target_server_version"),
            server_compatibility=raw.get("server_compatibility"),
            uptime_seconds=raw.get("uptime_seconds"),
        )

    def healthz(self) -> bool:
        try:
            return self._request("GET", "/healthz").is_success
        except MatrixError:
            return False

    def healthdata(self) -> dict[str, Any]:
        return self._request("GET", "/healthdata").json()

    # -- registry -----------------------------------------------------------

    def apply(self, yaml: str) -> ApplyOutcome:
        """Apply one asset, kind-sniffed by Matrix from its `kind:` line.

        Re-applying identical bytes is `unchanged=True`, not an error: that is
        ordinary GitOps. The same version with DIFFERENT bytes is refused,
        because a version is provenance — sealed evidence cites it.
        """
        raw = self._request(
            "POST",
            "/v1/assets",
            content=yaml.encode(),
            headers={"content-type": "text/yaml"},
        ).json()
        return ApplyOutcome(
            asset_ref=raw.get("asset_ref", ""),
            kind=raw.get("kind", ""),
            unchanged=bool(raw.get("unchanged", False)),
        )

    def validate(self, yaml: str) -> Validation:
        """Matrix's own verdict and its findings.

        `valid` is the SERVICE's answer, not `not findings`: three codes are
        advisory and a valid asset can carry them.
        """
        raw = self._request(
            "POST",
            "/v1/assets/validate",
            content=yaml.encode(),
            headers={"content-type": "text/yaml"},
        ).json()
        return Validation(
            valid=bool(raw.get("valid", False)),
            findings=[_finding(f) for f in raw.get("findings", [])],
        )

    def list_assets(
        self, kind: str, *, all_versions: bool = False
    ) -> list[dict[str, Any]]:
        """`kind` is a route segment: datasources, contracts, mappings,
        metricviews, dataviews."""
        raw = self._request(
            "GET",
            f"/v1/{kind}",
            params={"all_versions": "true"} if all_versions else None,
        ).json()
        return list(raw.get("assets", []))

    def get_yaml(self, kind: str, name: str) -> str:
        """The applied YAML, verbatim — the bytes Matrix stored, not a
        re-serialisation of a parse."""
        return self._request("GET", f"/v1/{kind}/{name}").text

    # -- operations ---------------------------------------------------------

    def introspect(self, source: str) -> dict[str, Any]:
        return self._request("POST", f"/v1/datasources/{source}/introspect").json()

    def probe(self, source: str) -> dict[str, Any]:
        """Reachability now. A refusal is an ANSWER here — `reachable: false`
        with a typed reason — not an exception."""
        return self._request("POST", f"/v1/datasources/{source}/probe").json()

    def sync(self, source: str) -> JobAccepted:
        raw = self._request("POST", f"/v1/datasources/{source}/sync").json()
        return JobAccepted(
            accepted=int(raw.get("accepted", 0)),
            jobs=list(raw.get("jobs", [])),
            detail=str(raw.get("detail", "")),
        )

    def verify(self, contract: str) -> VerifyOutcome:
        """Run a query contract's verified questions — its regression suite.

        The call succeeding and the CONTRACT passing are different things:
        check `failed`. `mxctl` exits 3 on a non-zero `failed` for exactly
        this reason, so CI can tell a broken contract from a broken command.
        """
        return _verify(self._request("POST", f"/v1/contracts/{contract}/verify").json())

    def verify_view(self, view: str) -> VerifyOutcome:
        """The same for a metric view or a native data view, recording the
        definition fingerprint the questions ran under.

        A metric view first; a data view when there is none by that name.
        """
        try:
            return _verify(
                self._request("POST", f"/v1/metricviews/{view}/verify").json()
            )
        except MatrixError as exc:
            if not _looks_like_no_such_view(exc):
                raise
            return _verify(self._request("POST", f"/v1/dataviews/{view}/verify").json())

    def reconcile(self, mapping: str) -> JobAccepted:
        raw = self._request("POST", f"/v1/mappings/{mapping}/run").json()
        return JobAccepted(
            accepted=int(raw.get("accepted", 0)),
            jobs=list(raw.get("jobs", [])),
            detail=str(raw.get("detail", "")),
        )

    # -- promotion ----------------------------------------------------------

    def promotion_status(self, mapping: str) -> PromotionStatus:
        raw = self._request("GET", f"/v1/mappings/{mapping}/promotion").json()
        return _promotion(raw, mapping)

    def gate_history(self, mapping: str, *, limit: int | None = None) -> dict[str, Any]:
        return self._request(
            "GET",
            f"/v1/mappings/{mapping}/gate-history",
            params={"limit": limit} if limit else None,
        ).json()

    def promote(
        self,
        mapping: str,
        *,
        decision_id: str,
        actor: str | None = None,
        reason: str | None = None,
    ) -> PromotionStatus:
        """Let a mapping's claims reach the ledger.

        The gates (identity precision, value conformance) are checked by
        MATRIX at the decision, not here: a client that pre-checked them
        would be a second opinion nobody audited.
        """
        # `actor` is optional on the wire: the service records `tenant:role`
        # when it is absent, which is a truthful default. Naming a person is
        # better, so the parameter stays -- but requiring it here while the API
        # does not would be this client inventing a rule.
        body: dict[str, Any] = {"decision_id": decision_id}
        if actor:
            body["actor"] = actor
        if reason:
            body["reason"] = reason
        raw = self._request("POST", f"/v1/mappings/{mapping}/promote", json=body).json()
        return _promotion(raw, mapping)

    def demote(self, mapping: str, *, decision_id: str) -> PromotionStatus:
        raw = self._request(
            "POST", f"/v1/mappings/{mapping}/demote", json={"decision_id": decision_id}
        ).json()
        # The route answers the full status. Fabricating `promoted=False`
        # instead of decoding it would report success the service never
        # confirmed.
        return _promotion(raw, mapping)

    def rollback(self, mapping: str, *, decision_id: str) -> dict[str, Any]:
        """Undo what a promoted mapping wrote — by SUPERSESSION, never by
        deletion. History is not rewritten."""
        return self._request(
            "POST",
            f"/v1/mappings/{mapping}/rollback",
            json={"decision_id": decision_id},
        ).json()

    # -- journal ------------------------------------------------------------

    def journal(self, *, limit: int = 50) -> list[dict[str, Any]]:
        """Every operation, redacted by default: parameters and results never
        appear, only what happened and how it ended."""
        raw = self._request("GET", "/v1/journal", params={"limit": limit}).json()
        # One key, the one the service emits. Probing for `records` and
        # `journal` too looked defensive and was superstition: neither has ever
        # been sent, and a fallback nobody can reach hides a rename instead of
        # surviving one.
        return list(raw.get("entries", []))


class AsyncMatrixClient(_Base):
    """The same surface, awaited. Kept deliberately parallel: a method that
    exists on one and not the other is a trap for a caller porting between
    them."""

    def __init__(self, base_url: str, **kwargs: Any) -> None:
        super().__init__(base_url, **kwargs)
        self._http = httpx.AsyncClient(timeout=self._timeout, headers=self._headers)

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(self, *_: object) -> None:
        await self.aclose()

    async def aclose(self) -> None:
        await self._http.aclose()

    async def _request(self, method: str, path: str, **kwargs: Any) -> httpx.Response:
        try:
            response = await self._http.request(method, self._url(path), **kwargs)
        except httpx.HTTPError as exc:
            raise MatrixError(str(exc), refusal_class="unavailable") from exc
        _raise_for(response)
        return response

    async def version(self) -> Version:
        raw = (await self._request("GET", "/version")).json()
        return Version(
            version=raw.get("version", ""),
            contract_version=raw.get("contract_version", ""),
            role=raw.get("role", ""),
            server_version=raw.get("server_version"),
            target_server_version=raw.get("target_server_version"),
            server_compatibility=raw.get("server_compatibility"),
            uptime_seconds=raw.get("uptime_seconds"),
        )

    async def healthz(self) -> bool:
        try:
            return (await self._request("GET", "/healthz")).is_success
        except MatrixError:
            return False

    async def apply(self, yaml: str) -> ApplyOutcome:
        raw = (
            await self._request(
                "POST",
                "/v1/assets",
                content=yaml.encode(),
                headers={"content-type": "text/yaml"},
            )
        ).json()
        return ApplyOutcome(
            asset_ref=raw.get("asset_ref", ""),
            kind=raw.get("kind", ""),
            unchanged=bool(raw.get("unchanged", False)),
        )

    async def validate(self, yaml: str) -> Validation:
        raw = (
            await self._request(
                "POST",
                "/v1/assets/validate",
                content=yaml.encode(),
                headers={"content-type": "text/yaml"},
            )
        ).json()
        return Validation(
            valid=bool(raw.get("valid", False)),
            findings=[_finding(f) for f in raw.get("findings", [])],
        )

    async def list_assets(
        self, kind: str, *, all_versions: bool = False
    ) -> list[dict[str, Any]]:
        raw = (
            await self._request(
                "GET",
                f"/v1/{kind}",
                params={"all_versions": "true"} if all_versions else None,
            )
        ).json()
        return list(raw.get("assets", []))

    async def get_yaml(self, kind: str, name: str) -> str:
        return (await self._request("GET", f"/v1/{kind}/{name}")).text

    async def introspect(self, source: str) -> dict[str, Any]:
        return (
            await self._request("POST", f"/v1/datasources/{source}/introspect")
        ).json()

    async def probe(self, source: str) -> dict[str, Any]:
        return (await self._request("POST", f"/v1/datasources/{source}/probe")).json()

    async def sync(self, source: str) -> JobAccepted:
        raw = (await self._request("POST", f"/v1/datasources/{source}/sync")).json()
        return JobAccepted(
            accepted=int(raw.get("accepted", 0)),
            jobs=list(raw.get("jobs", [])),
            detail=str(raw.get("detail", "")),
        )

    async def verify(self, contract: str) -> VerifyOutcome:
        return _verify(
            (await self._request("POST", f"/v1/contracts/{contract}/verify")).json()
        )

    async def verify_view(self, view: str) -> VerifyOutcome:
        try:
            return _verify(
                (await self._request("POST", f"/v1/metricviews/{view}/verify")).json()
            )
        except MatrixError as exc:
            if not _looks_like_no_such_view(exc):
                raise
            return _verify(
                (await self._request("POST", f"/v1/dataviews/{view}/verify")).json()
            )

    async def reconcile(self, mapping: str) -> JobAccepted:
        raw = (await self._request("POST", f"/v1/mappings/{mapping}/run")).json()
        return JobAccepted(
            accepted=int(raw.get("accepted", 0)),
            jobs=list(raw.get("jobs", [])),
            detail=str(raw.get("detail", "")),
        )

    async def promotion_status(self, mapping: str) -> PromotionStatus:
        raw = (await self._request("GET", f"/v1/mappings/{mapping}/promotion")).json()
        return _promotion(raw, mapping)

    async def healthdata(self) -> dict[str, Any]:
        return (await self._request("GET", "/healthdata")).json()

    async def gate_history(
        self, mapping: str, *, limit: int | None = None
    ) -> dict[str, Any]:
        return (
            await self._request(
                "GET",
                f"/v1/mappings/{mapping}/gate-history",
                params={"limit": limit} if limit else None,
            )
        ).json()

    async def promote(
        self,
        mapping: str,
        *,
        decision_id: str,
        actor: str | None = None,
        reason: str | None = None,
    ) -> PromotionStatus:
        body: dict[str, Any] = {"decision_id": decision_id}
        if actor:
            body["actor"] = actor
        if reason:
            body["reason"] = reason
        raw = (
            await self._request("POST", f"/v1/mappings/{mapping}/promote", json=body)
        ).json()
        return _promotion(raw, mapping)

    async def demote(self, mapping: str, *, decision_id: str) -> PromotionStatus:
        raw = (
            await self._request(
                "POST",
                f"/v1/mappings/{mapping}/demote",
                json={"decision_id": decision_id},
            )
        ).json()
        return _promotion(raw, mapping)

    async def rollback(self, mapping: str, *, decision_id: str) -> dict[str, Any]:
        return (
            await self._request(
                "POST",
                f"/v1/mappings/{mapping}/rollback",
                json={"decision_id": decision_id},
            )
        ).json()

    async def journal(self, *, limit: int = 50) -> list[dict[str, Any]]:
        raw = (
            await self._request("GET", "/v1/journal", params={"limit": limit})
        ).json()
        return list(raw.get("entries", []))
