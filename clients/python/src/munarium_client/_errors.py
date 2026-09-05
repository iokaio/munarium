# SPDX-License-Identifier: Apache-2.0
"""One typed error surface keyed on the problem-slug registry
(server/docs/api/errors.md). REST decodes application/problem+json; gRPC
decodes the google.rpc.ErrorInfo detail in grpc-status-details-bin.
No English message text is ever parsed."""

from __future__ import annotations

from typing import Any

from .models import GateFinding


class MunariumError(Exception):
    """Root of every error this client raises."""

    slug: str | None = None
    #: Retrying the SAME request (same idempotency key) is safe and may
    #: succeed. Head conflicts are retryable too but need a REBUILT request.
    transient: bool = False


class HeadConflictError(MunariumError):
    """Optimistic expected_head mismatch — normal and retryable: re-read
    head, re-decide, retry (or use `propose_claim_with_retry`). ``actual == 0``
    means the transport carried no structured seqs — re-read the head."""

    slug = "head-conflict"

    def __init__(self, expected: int, actual: int, detail: str = "") -> None:
        super().__init__(detail or f"head conflict: expected seq {expected}, actual {actual}")
        self.expected = expected
        self.actual = actual


class PolicyRejectionError(MunariumError):
    """Block-severity gate findings on a non-claim path. NOTE: a gated
    propose_claim/append_events does NOT raise — the claim is recorded
    disputed and returned with findings (success, invariant #1). On gRPC the
    findings list is size-capped: ``findings_total`` is the real count and
    ``findings_truncated`` marks a capped list."""

    slug = "policy-rejection"

    def __init__(
        self,
        findings: list[GateFinding],
        findings_total: int,
        findings_truncated: bool,
        detail: str = "",
    ) -> None:
        super().__init__(detail or f"policy rejection: {len(findings)} finding(s)")
        self.findings = findings
        self.findings_total = findings_total
        self.findings_truncated = findings_truncated


class ShapeViolationError(MunariumError):
    slug = "shape-violation"

    def __init__(self, shape_ref: str, detail: str) -> None:
        super().__init__(detail)
        self.shape_ref = shape_ref


class IdempotencyMismatchError(MunariumError):
    slug = "idempotency-mismatch"


class NotFoundError(MunariumError):
    slug = "not-found"

    def __init__(self, kind: str, id: str, detail: str = "") -> None:
        super().__init__(detail or f"not found: {kind} {id}")
        self.kind = kind
        self.id = id


class InvalidInputError(MunariumError):
    slug = "invalid-input"


class UnauthenticatedError(MunariumError):
    slug = "unauthenticated"


class ForbiddenError(MunariumError):
    slug = "forbidden"


class RateLimitedError(MunariumError):
    """NOT auto-retried — honor ``retry_after`` (seconds) in your pacing."""

    slug = "rate-limited"

    def __init__(self, detail: str, retry_after: float | None = None) -> None:
        super().__init__(detail)
        self.retry_after = retry_after


class OverloadedError(MunariumError):
    """Load-shed / graceful drain — transient, retried on read paths."""

    slug = "overloaded"
    transient = True


class RunLockedError(MunariumError):
    """Another run holds this runbook's run lock (409 / gRPC ABORTED with
    reason "run-locked", 2026-08-17). The server rejected the request
    BEFORE executing anything, and the lock clears when the holding run
    finishes — retryable in YOUR OWN pacing, like ``RateLimitedError``, and
    for the same reason deliberately NOT transient: a run lock is held for
    a whole run (minutes), so sub-second auto-retry would be futile churn
    that masks the typed signal."""

    slug = "run-locked"
    transient = False


class StorageError(MunariumError):
    slug = "storage-error"


class ProviderError(MunariumError):
    slug = "provider-error"


class UnsupportedError(MunariumError):
    """The operation has no RPC/route on this transport (e.g. index builds
    are REST-only; gRPC provider calls cannot record invocation provenance)."""


class TransportError(MunariumError):
    """Connection-level failure — the request may never have arrived."""

    transient = True


class UnexpectedError(MunariumError):
    def __init__(self, detail: str, status: int | None = None) -> None:
        super().__init__(detail)
        self.status = status
        # 5xx gateway statuses are transient for the read-retry class.
        self.transient = status in (502, 503, 504)


def _parse_findings(raw: Any) -> list[GateFinding]:
    if not isinstance(raw, list):
        return []
    out: list[GateFinding] = []
    for item in raw:
        try:
            out.append(GateFinding.model_validate(item))
        except Exception:  # noqa: BLE001 - a bad finding never masks the error
            continue
    return out


def error_from_parts(
    slug: str,
    detail: str,
    ext: dict[str, Any],
    status: int | None = None,
    retry_after: float | None = None,
) -> MunariumError:
    """Build the typed error for a problem slug + extension members. The
    member NAMES are identical on both transports (errors.md)."""
    if slug == "head-conflict":
        return HeadConflictError(int(ext.get("expected") or 0), int(ext.get("actual") or 0), detail)
    if slug == "policy-rejection":
        findings = _parse_findings(ext.get("gate_findings"))
        total = int(ext.get("findings_total") or len(findings))
        truncated = str(ext.get("findings_truncated", "")).lower() == "true" or (
            ext.get("findings_truncated") is True
        )
        return PolicyRejectionError(findings, total, truncated, detail)
    if slug == "shape-violation":
        return ShapeViolationError(str(ext.get("shape_ref") or ""), detail)
    if slug == "idempotency-mismatch":
        return IdempotencyMismatchError(detail)
    if slug == "not-found":
        return NotFoundError(str(ext.get("kind") or "resource"), str(ext.get("id") or ""), detail)
    if slug == "invalid-input":
        return InvalidInputError(detail)
    if slug == "unauthenticated":
        return UnauthenticatedError(detail)
    if slug == "forbidden":
        return ForbiddenError(detail)
    if slug == "rate-limited":
        return RateLimitedError(detail, retry_after)
    if slug == "overloaded":
        return OverloadedError(detail)
    if slug == "storage-error":
        return StorageError(detail)
    if slug == "provider-error":
        return ProviderError(detail)
    # platform identity/lifecycle slugs — mapped to the existing kinds by status
    # class so re-auth/permission logic keeps working (token expiry/revocation
    # is Unauthenticated so a caller can refresh; runbook-removed is a 410
    # surfaced as NotFound).
    if slug == "uid-required" or slug == "removal-not-confirmed":
        return InvalidInputError(detail)
    if slug == "token-expired" or slug == "token-revoked":
        return UnauthenticatedError(detail)
    if slug in ("uid-mismatch", "scope-missing", "override-not-allowed"):
        return ForbiddenError(detail)
    # 2026-08-17/19 lifecycle slugs (sessions, run lock, authoring).
    # session-not-open / authoring-draft-invalid follow the same status-class
    # convention as removal-not-confirmed; run-locked is its own kind because
    # its RETRYABILITY is semantic — before this mapping it decoded as
    # Unexpected, hiding that a later re-run succeeds once the lock clears.
    if slug in ("session-not-open", "authoring-draft-invalid"):
        return InvalidInputError(detail)
    if slug == "run-locked":
        return RunLockedError(detail)
    if slug == "runbook-removed":
        return NotFoundError(str(ext.get("kind") or "runbook"), str(ext.get("id") or ""), detail)
    return UnexpectedError(detail, status)


def error_from_problem(status: int, body: Any, retry_after: float | None = None) -> MunariumError:
    """Decode a REST problem+json body."""
    if not isinstance(body, dict):
        return UnexpectedError(f"non-problem error body (HTTP {status})", status)
    slug = str(body.get("type", "")).rsplit("/", 1)[-1]
    detail = str(body.get("detail") or "")
    ext = dict(body)
    # REST bodies carry the full findings list.
    if "gate_findings" in ext and "findings_total" not in ext:
        raw = ext.get("gate_findings")
        ext["findings_total"] = len(raw) if isinstance(raw, list) else 0
    return error_from_parts(slug, detail, ext, status=status, retry_after=retry_after)


ERROR_DOMAIN = "mmp.ioka.io"


def error_from_grpc(code_name: str, detail: str, error_info: Any | None) -> MunariumError:
    """Decode a gRPC status. ``error_info`` is a google.rpc.ErrorInfo message
    (or None when no structured details rode the trailer)."""
    if error_info is not None and getattr(error_info, "domain", "") == ERROR_DOMAIN:
        md = dict(error_info.metadata)
        ext: dict[str, Any] = dict(md)
        if "gate_findings" in md:
            import json

            try:
                ext["gate_findings"] = json.loads(md["gate_findings"])
            except ValueError:
                ext["gate_findings"] = []
        return error_from_parts(error_info.reason, detail, ext)

    # Code-based fallback (intermediary-minted statuses without details).
    match code_name:
        case "ABORTED":
            # The head-conflict code; 0/0 = re-read the head yourself.
            return HeadConflictError(0, 0, detail)
        case "NOT_FOUND":
            return NotFoundError("resource", "", detail)
        case "INVALID_ARGUMENT":
            return InvalidInputError(detail)
        case "UNAUTHENTICATED":
            return UnauthenticatedError(detail)
        case "PERMISSION_DENIED":
            return ForbiddenError(detail)
        case "RESOURCE_EXHAUSTED":
            return RateLimitedError(detail)
        case "UNAVAILABLE":
            return TransportError(detail)
        case "INTERNAL":
            return StorageError(detail)
        case _:
            return UnexpectedError(f"{code_name}: {detail}")


#: The promise statuses the server matches on. It FILTERS an unrecognized
#: value instead of erroring, so an unvalidated typo returns an empty list —
#: a silent wrong answer about outstanding obligations.
PROMISE_STATUSES = ("open", "fulfilled", "expired", "violated")


def check_promise_status(status: str | None) -> None:
    """Reject a promise status filter the server would silently drop."""
    if status is not None and status not in PROMISE_STATUSES:
        raise InvalidInputError(
            f"unknown promise status {status!r} ({' | '.join(PROMISE_STATUSES)})",
        )


#: The server's per-chunk file cap on bulk_chunk (and batch ingest).
BULK_MAX_FILES_PER_CHUNK = 500


def check_bulk_files(what: str, n: int) -> None:
    """Reject an over-cap file list before it ships 256 MiB the server will
    refuse. ``what`` names the calling surface ("batch" / "bulk chunk") so
    the error speaks the API the caller actually used."""
    if n == 0 or n > BULK_MAX_FILES_PER_CHUNK:
        raise InvalidInputError(f"{what} must carry 1..={BULK_MAX_FILES_PER_CHUNK} files (got {n})")
