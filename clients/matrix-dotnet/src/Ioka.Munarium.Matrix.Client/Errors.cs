// SPDX-License-Identifier: Apache-2.0
// One error type, because Matrix has one error vocabulary. The server's .NET
// client fans out into a class per problem slug; that is right there, where
// the slugs are an open registry a caller branches on structurally. Matrix's
// taxonomy is the other shape: a CLOSED six-member `class` plus an open
// `code`, and the class is what decides behaviour. A subclass per class would
// be six types whose only difference is a string, and a subclass per code
// would be a type hierarchy that has to grow every time a refusal message is
// added — so the discrimination stays in properties.

using System.Text.Json;

namespace Ioka.Munarium.Matrix.Client;

/// <summary>A refusal, or a transport failure.</summary>
/// <remarks>
/// Matrix answers a refusal as RFC 9457 problem+json with a <c>refusal</c>
/// object carrying the CLASS and the CODE — the closed vocabulary the whole
/// system is built on. Those are surfaced as properties rather than flattened
/// into the message, because a caller that must distinguish "not covered"
/// from "budget exhausted" should not be parsing prose to do it.
/// </remarks>
public sealed class MatrixException : Exception
{
    internal MatrixException(
        string message,
        int? status = null,
        string? code = null,
        string? refusalClass = null,
        string? detail = null,
        TimeSpan? retryAfter = null,
        Exception? inner = null)
        : base(message, inner)
    {
        Status = status;
        Code = code;
        RefusalClass = refusalClass;
        Detail = detail;
        RetryAfter = retryAfter;
    }

    /// <summary>The HTTP status, when one was received. Null means the
    /// request never got an answer at all.</summary>
    public int? Status { get; }

    /// <summary>The refusal's open code — <c>budget_exceeded</c>,
    /// <c>not_covered</c>, <c>schema_drift</c>. What an operator reads.</summary>
    public string? Code { get; }

    /// <summary>The refusal's CLOSED class: not_covered | unavailable |
    /// denied | incomplete | invalid | exhausted. What a program switches
    /// on. Adding a member is a major contract bump, which is why this stays
    /// a string and not a client-side enum — an enum here would turn a
    /// forward-compatible seventh class into a deserialization failure in
    /// every deployed application.</summary>
    public string? RefusalClass { get; }

    /// <summary>The problem's <c>detail</c> member, verbatim.</summary>
    public string? Detail { get; }

    /// <summary>How long the refusal asked the caller to wait, when it said.
    /// Only <c>exhausted</c> refusals carry one today. The Python client
    /// drops this field; it costs nothing to keep, and a caller pacing a
    /// budget refusal by guesswork is pacing it worse than the service
    /// already told it to.</summary>
    public TimeSpan? RetryAfter { get; }

    /// <summary>Whether retrying the SAME request could plausibly succeed.
    /// <c>unavailable</c> and <c>exhausted</c> are states of the world; the
    /// rest are statements about the request or the assets, and repeating it
    /// changes nothing. A caller that retries a <c>denied</c> is hammering a
    /// door that is locked on purpose.</summary>
    public bool Retryable => RefusalClass is "unavailable" or "exhausted";

    /// <summary>True when this error is the service saying the named asset is
    /// not registered — which it spells two different ways, so
    /// <see cref="MatrixClient.VerifyViewAsync"/> cannot key on the status
    /// alone. A store miss surfaces as a bare 404 problem with no refusal
    /// object; a miss that went through the asset loader is turned into a
    /// <c>not_covered</c> refusal, and <c>not_covered</c> maps to 422.</summary>
    internal bool IsNoSuchAsset => Status == 404 || (Status == 422 && Code == "not_covered");
}

internal static class Errors
{
    /// <summary>Decode a non-success response.</summary>
    /// <remarks>
    /// The one subtlety worth stating: <c>refusal</c> is a free-form JSON
    /// value on the wire, not always the typed refusal object. An asset that
    /// fails validation puts the findings ARRAY there instead. Reading it as
    /// an object without checking would turn a 422 on a bad asset — the most
    /// ordinary failure this client sees — into a crash inside the error
    /// path, which is the worst place for one.
    /// </remarks>
    internal static MatrixException FromProblem(int status, string body)
    {
        string? detail = null, title = null, code = null, refusalClass = null;
        TimeSpan? retryAfter = null;
        try
        {
            using var doc = JsonDocument.Parse(body);
            if (doc.RootElement.ValueKind == JsonValueKind.Object)
            {
                var root = doc.RootElement;
                detail = Str(root, "detail");
                title = Str(root, "title");
                if (root.TryGetProperty("refusal", out var refusal)
                    && refusal.ValueKind == JsonValueKind.Object)
                {
                    code = Str(refusal, "code");
                    refusalClass = Str(refusal, "class");
                    if (refusal.TryGetProperty("retry_after_seconds", out var after)
                        && after.TryGetUInt64(out var seconds))
                    {
                        retryAfter = TimeSpan.FromSeconds(seconds);
                    }
                }
            }
        }
        catch (JsonException)
        {
            // A non-JSON body (a gateway's HTML, a truncated stream) still has
            // to become the one typed error; the status is what survives.
        }

        return new MatrixException(
            detail ?? title ?? $"matrix answered {status}",
            status, code, refusalClass, detail, retryAfter);
    }

    /// <summary>A transport failure is <c>unavailable</c>: the request did
    /// not get an answer, which is a state of the world and therefore
    /// retryable, exactly like a refusal that says the source is down.</summary>
    internal static MatrixException FromTransport(Exception e) =>
        new(e.Message, refusalClass: "unavailable", inner: e);

    private static string? Str(JsonElement obj, string name) =>
        obj.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String
            ? v.GetString()
            : null;
}
