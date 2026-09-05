// SPDX-License-Identifier: Apache-2.0
// One typed error surface keyed on the problem-slug registry
// (server/docs/api/errors.md). REST decodes application/problem+json; gRPC
// decodes the google.rpc.ErrorInfo detail in grpc-status-details-bin.
// No English message text is ever parsed.

namespace Ioka.Munarium.Client;

/// <summary>Root of every error this client throws.</summary>
public class MunariumException : Exception
{
    public MunariumException(string message) : base(message) { }

    /// <summary>Registry slug, when this error maps to one.</summary>
    public virtual string? Slug => null;

    /// <summary>Retrying the SAME request (same idempotency key) is safe and
    /// may succeed. Head conflicts are retryable too but need a REBUILT
    /// request — see <see cref="MunariumClient.ProposeClaimWithRetryAsync"/>.</summary>
    public virtual bool Transient => false;
}

/// <summary>Optimistic expected_head mismatch — normal and retryable:
/// re-read head, re-decide, retry. <c>Actual == 0</c> means the transport
/// carried no structured seqs — re-read the head yourself.</summary>
public sealed class HeadConflictException(ulong expected, ulong actual, string? detail = null)
    : MunariumException(detail ?? $"head conflict: expected seq {expected}, actual {actual}")
{
    public ulong Expected { get; } = expected;
    public ulong Actual { get; } = actual;
    public override string? Slug => "head-conflict";
}

/// <summary>Block-severity gate findings on a non-claim path. NOTE: a gated
/// ProposeClaim/AppendEvents does NOT throw — the claim is recorded disputed
/// and returned with findings (success, invariant #1). On gRPC the findings
/// list is size-capped: <see cref="FindingsTotal"/> is the real count and
/// <see cref="FindingsTruncated"/> marks a capped list.</summary>
public sealed class PolicyRejectionException(
    IReadOnlyList<GateFinding> findings, ulong findingsTotal, bool findingsTruncated, string? detail = null)
    : MunariumException(detail ?? $"policy rejection: {findings.Count} finding(s)")
{
    public IReadOnlyList<GateFinding> Findings { get; } = findings;
    public ulong FindingsTotal { get; } = findingsTotal;
    public bool FindingsTruncated { get; } = findingsTruncated;
    public override string? Slug => "policy-rejection";
}

public sealed class ShapeViolationException(string shapeRef, string detail)
    : MunariumException(detail)
{
    public string ShapeRef { get; } = shapeRef;
    public override string? Slug => "shape-violation";
}

public sealed class IdempotencyMismatchException(string detail) : MunariumException(detail)
{
    public override string? Slug => "idempotency-mismatch";
}

public sealed class MunariumNotFoundException(string kind, string id, string? detail = null)
    : MunariumException(string.IsNullOrEmpty(detail) ? $"not found: {kind} {id}" : detail)
{
    public string Kind { get; } = kind;
    public string Id { get; } = id;
    public override string? Slug => "not-found";
}

public sealed class InvalidInputException(string detail) : MunariumException(detail)
{
    public override string? Slug => "invalid-input";
}

public sealed class UnauthenticatedException(string detail) : MunariumException(detail)
{
    public override string? Slug => "unauthenticated";
}

public sealed class ForbiddenException(string detail) : MunariumException(detail)
{
    public override string? Slug => "forbidden";
}

/// <summary>NOT auto-retried — honor <see cref="RetryAfter"/> in your pacing.</summary>
public sealed class RateLimitedException(string detail, TimeSpan? retryAfter = null)
    : MunariumException(detail)
{
    public TimeSpan? RetryAfter { get; } = retryAfter;
    public override string? Slug => "rate-limited";
}

/// <summary>Load-shed / graceful drain — transient, retried on read paths.</summary>
public sealed class OverloadedException(string detail) : MunariumException(detail)
{
    public override string? Slug => "overloaded";
    public override bool Transient => true;
}

/// <summary>Another run holds this runbook's run lock (409 / gRPC ABORTED
/// with reason "run-locked", 2026-08-17). The server rejected the request
/// BEFORE executing anything, and the lock clears when the holding run
/// finishes — retryable in YOUR OWN pacing, like
/// <see cref="RateLimitedException"/>, and for the same reason deliberately
/// NOT transient: a run lock is held for a whole run (minutes), so
/// sub-second auto-retry would be futile churn that masks the typed
/// signal.</summary>
public sealed class RunLockedException(string detail) : MunariumException(detail)
{
    public override string? Slug => "run-locked";
}

public sealed class MunariumStorageException(string detail) : MunariumException(detail)
{
    public override string? Slug => "storage-error";
}

public sealed class MunariumProviderException(string detail) : MunariumException(detail)
{
    public override string? Slug => "provider-error";
}

/// <summary>The operation has no RPC/route on this transport (e.g. index
/// builds are REST-only; gRPC provider calls cannot record invocation
/// provenance).</summary>
public sealed class UnsupportedTransportException(string detail) : MunariumException(detail);

/// <summary>Connection-level failure — the request may never have arrived.</summary>
public sealed class MunariumTransportException(string detail, bool mayHaveReachedServer = true)
    : MunariumException(detail)
{
    public override bool Transient => true;

    /// <summary>True when the request may already have reached the server (a
    /// timeout or reset on an established connection). The server records an
    /// idempotency key only AFTER a command completes, so a command failing
    /// this way is NOT auto-retried: a retry could overtake an in-flight
    /// attempt and execute it twice. Reads retry regardless. False means the
    /// request provably never left, and re-sending is always safe.</summary>
    public bool MayHaveReachedServer { get; } = mayHaveReachedServer;
}

/// <summary>An error response that did not match the registry.</summary>
public sealed class UnexpectedServerException(string detail, int? status = null)
    : MunariumException(detail)
{
    public int? Status { get; } = status;
    /// <summary>5xx gateway statuses are transient for the read-retry class.</summary>
    public override bool Transient => Status is >= 502 and <= 504;
}
