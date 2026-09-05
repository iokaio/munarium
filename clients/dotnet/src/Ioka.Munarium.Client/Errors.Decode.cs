// SPDX-License-Identifier: Apache-2.0
// Decoding: REST problem+json and gRPC ErrorInfo both normalize into the
// SAME Problem shape, and FromParts is the single construction point for
// every typed error — defaulting rules exist exactly once.

using System.Text.Json;
using Google.Protobuf;
using Grpc.Core;

namespace Ioka.Munarium.Client;

internal static class Errors
{
    private const string Domain = "mmp.ioka.io";

    internal static MunariumException FromParts(
        string slug, string detail, Problem ext, int? status = null, TimeSpan? retryAfter = null)
        => slug switch
        {
            "head-conflict" => new HeadConflictException(ext.Expected ?? 0, ext.Actual ?? 0, detail),
            "policy-rejection" => new PolicyRejectionException(
                ext.GateFindings ?? [],
                ext.FindingsTotal ?? (ulong)(ext.GateFindings?.Count ?? 0),
                ext.FindingsTruncated ?? false,
                detail),
            "shape-violation" => new ShapeViolationException(ext.ShapeRef ?? "", detail),
            "idempotency-mismatch" => new IdempotencyMismatchException(detail),
            "not-found" => new MunariumNotFoundException(ext.Kind ?? "resource", ext.Id ?? "", detail),
            "invalid-input" => new InvalidInputException(detail),
            "unauthenticated" => new UnauthenticatedException(detail),
            "forbidden" => new ForbiddenException(detail),
            "rate-limited" => new RateLimitedException(detail, retryAfter),
            "overloaded" => new OverloadedException(detail),
            "storage-error" => new MunariumStorageException(detail),
            "provider-error" => new MunariumProviderException(detail),
            // platform identity/lifecycle slugs — mapped to the existing kinds
            // by status class so re-auth/permission logic keeps working (token
            // expiry/revocation is Unauthenticated so a caller can refresh;
            // runbook-removed is a 410 surfaced as NotFound).
            "uid-required" or "removal-not-confirmed" => new InvalidInputException(detail),
            // 2026-08-17/19 lifecycle slugs (sessions, run lock, authoring).
            // session-not-open / authoring-draft-invalid follow the same
            // status-class convention as removal-not-confirmed; run-locked is
            // its own kind because its RETRYABILITY is semantic — before this
            // mapping it decoded as Unexpected, hiding that a later re-run
            // succeeds once the holding run finishes.
            "session-not-open" or "authoring-draft-invalid" => new InvalidInputException(detail),
            "run-locked" => new RunLockedException(detail),
            "token-expired" or "token-revoked" => new UnauthenticatedException(detail),
            "uid-mismatch" or "scope-missing" or "override-not-allowed"
                => new ForbiddenException(detail),
            "runbook-removed" => new MunariumNotFoundException(ext.Kind ?? "runbook", ext.Id ?? "", detail),
            _ => new UnexpectedServerException(detail, status),
        };

    /// <summary>Decode a problem+json error body. <paramref name="status"/>
    /// is the transport's HTTP status; null when the carrier has none (the
    /// SSE error event), in which case the body's own <c>status</c> member
    /// stands in.</summary>
    internal static MunariumException FromProblem(int? status, string body, TimeSpan? retryAfter)
    {
        var http = status is null ? "" : $" (HTTP {status})";
        Problem? problem;
        try
        {
            problem = JsonSerializer.Deserialize(body, MunariumJsonContext.Default.Problem);
        }
        catch (JsonException)
        {
            return new UnexpectedServerException($"non-JSON error body{http}", status);
        }
        if (problem is null)
        {
            return new UnexpectedServerException($"empty error body{http}", status);
        }
        var slug = (problem.Type ?? "").Split('/')[^1];
        return FromParts(slug, problem.Detail ?? "", problem, status ?? problem.Status, retryAfter);
    }

    /// <summary>Decode a gRPC RpcException via the ErrorInfo structured
    /// detail in grpc-status-details-bin (metadata member names are identical
    /// to the REST problem+json extensions); code-based fallback when details
    /// are absent (e.g. intermediary-minted statuses).</summary>
    internal static MunariumException FromRpc(RpcException e)
    {
        var detail = e.Status.Detail;
        var info = TryGetErrorInfo(e);
        if (info is not null && info.Domain == Domain)
        {
            var md = info.Metadata;
            var problem = new Problem
            {
                Expected = ParseULong(md, "expected"),
                Actual = ParseULong(md, "actual"),
                GateFindings = ParseFindings(md),
                FindingsTotal = ParseULong(md, "findings_total"),
                FindingsTruncated = md.TryGetValue("findings_truncated", out var t) && t == "true",
                ShapeRef = md.GetValueOrDefault("shape_ref"),
                Kind = md.GetValueOrDefault("kind"),
                Id = md.GetValueOrDefault("id"),
            };
            // The server does not emit google.rpc.RetryInfo today, so
            // RateLimitedException.RetryAfter stays null on gRPC.
            return FromParts(info.Reason, detail, problem);
        }

        return e.StatusCode switch
        {
            // ABORTED is the head-conflict code (grpc.md); 0/0 = re-read.
            // Documented ambiguity shared by all clients: the server also
            // answers ABORTED for a held run lock (reason "run-locked"), and
            // WITHOUT the ErrorInfo detail (e.g. an intermediary-minted
            // status) the two are indistinguishable — the head-conflict
            // reading is kept because it is the older, far more common one.
            StatusCode.Aborted => new HeadConflictException(0, 0, detail),
            StatusCode.NotFound => new MunariumNotFoundException("resource", "", detail),
            StatusCode.InvalidArgument => new InvalidInputException(detail),
            StatusCode.Unauthenticated => new UnauthenticatedException(detail),
            StatusCode.PermissionDenied => new ForbiddenException(detail),
            StatusCode.ResourceExhausted => new RateLimitedException(detail),
            StatusCode.Unavailable => new MunariumTransportException(detail),
            // A per-attempt deadline is a transport-level timeout — the same
            // retry class as the REST request deadline.
            StatusCode.DeadlineExceeded => new MunariumTransportException(detail),
            StatusCode.Internal => new MunariumStorageException(detail),
            _ => new UnexpectedServerException($"{e.StatusCode}: {detail}"),
        };
    }

    private static ulong? ParseULong(IDictionary<string, string> md, string key) =>
        md.TryGetValue(key, out var raw) && ulong.TryParse(raw, out var value) ? value : null;

    private static IReadOnlyList<GateFinding>? ParseFindings(IDictionary<string, string> md)
    {
        if (!md.TryGetValue("gate_findings", out var json)) return null;
        try
        {
            return JsonSerializer.Deserialize(json, MunariumJsonContext.Default.ListGateFinding);
        }
        catch (JsonException)
        {
            return null; // a bad finding never masks the underlying error
        }
    }

    private static Google.Rpc.ErrorInfo? TryGetErrorInfo(RpcException e)
    {
        var entry = e.Trailers.FirstOrDefault(t => t.Key == "grpc-status-details-bin");
        if (entry is null) return null;
        try
        {
            var status = Google.Rpc.Status.Parser.ParseFrom(entry.ValueBytes);
            foreach (var any in status.Details)
            {
                if (any.TryUnpack<Google.Rpc.ErrorInfo>(out var info)) return info;
            }
        }
        catch (InvalidProtocolBufferException)
        {
            return null;
        }
        return null;
    }
}
