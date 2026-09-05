// SPDX-License-Identifier: Apache-2.0
// Transport-neutral client-side input guards, shared by BOTH transports so
// neither depends on the other: an input the server would silently
// mis-handle (filter instead of reject, or refuse only after 256 MiB
// shipped) is a typed InvalidInputException before any bytes leave.

namespace Ioka.Munarium.Client;

internal static class Validation
{
    private static readonly string[] PromiseStatuses =
        ["open", "fulfilled", "expired", "violated"];

    /// <summary>Reject a promise status filter the server would silently
    /// drop: it FILTERS an unrecognized value instead of erroring, so a typo
    /// returns an empty list — a silent wrong answer about outstanding
    /// obligations.</summary>
    internal static void CheckPromiseStatus(string? status)
    {
        if (status is not null && !PromiseStatuses.Contains(status))
        {
            throw new InvalidInputException(
                $"unknown promise status '{status}' ({string.Join(" | ", PromiseStatuses)})");
        }
    }

    /// <summary>Reject an over-cap file list before it ships 256 MiB the
    /// server will refuse. <paramref name="what"/> names the calling surface
    /// ("batch" / "bulk chunk") so the error speaks the API the caller
    /// actually used.</summary>
    internal static void CheckBulkFiles(string what, int count)
    {
        if (count is 0 or > IIngestPlane.BulkMaxFilesPerChunk)
        {
            throw new InvalidInputException(
                $"{what} must carry 1..={IIngestPlane.BulkMaxFilesPerChunk} files (got {count})");
        }
    }

    /// <summary>proto3 cannot carry an EXPLICIT empty list — an empty
    /// repeated field is indistinguishable from an absent one, and the
    /// server reads absent as "unset" (collections: matcher auto-bind;
    /// runbook_refs: any runbook) where REST reads <c>[]</c> as "none". So
    /// an explicit empty list on gRPC is rejected like the zero sentinels
    /// (<c>null</c> — omitted — stays fine).</summary>
    internal static void RejectEmptyList<T>(string name, IReadOnlyList<T>? value)
    {
        if (value is { Count: 0 })
        {
            throw new InvalidInputException(
                $"{name} = [] cannot be represented on the gRPC wire (proto3 reads an empty " +
                "list as 'absent', which the server treats differently from an explicit " +
                "empty list); omit it, or use the REST transport");
        }
    }
}
