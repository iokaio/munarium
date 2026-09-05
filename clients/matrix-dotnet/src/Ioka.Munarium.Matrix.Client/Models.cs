// SPDX-License-Identifier: Apache-2.0
// Typed wire models mirroring matrix/src/munarium-matrix-types/src/dto.rs (the
// JSON casing truth). System.Text.Json source generation keeps serialization
// AOT-safe; unknown members are ignored on read, so an additive Matrix field
// never breaks a deployed client.
//
// The typing rule, so the next reader does not have to infer it: a shape the
// contract PINS gets a record; a shape that is open, role-gated, or exists
// mainly to be read by a human (journal rows, introspection, probes, gate
// history, a rollback tally) arrives as a JsonElement. Inventing records for
// the second group would create a second normative copy of shapes that are
// allowed to grow, and the growth would land here as a silent field drop.

using System.Text.Json;
using System.Text.Json.Serialization;

namespace Ioka.Munarium.Matrix.Client;

/// <summary>What <c>GET /version</c> says about this Matrix and the server it
/// seals into.</summary>
/// <remarks>Named <c>MatrixVersion</c> rather than <c>Version</c> only so it
/// does not shadow <see cref="System.Version"/> in a consumer's file.</remarks>
public sealed record MatrixVersion
{
    [JsonPropertyName("version")] public string Version { get; init; } = "";
    [JsonPropertyName("contract_version")] public string ContractVersion { get; init; } = "";

    /// <summary>Which surface this process serves: all | query | sync |
    /// reconcile | control. A role gates routes structurally, so a 404 from a
    /// Matrix is sometimes a statement about the deployment, not the URL.</summary>
    [JsonPropertyName("role")] public string Role { get; init; } = "";

    [JsonPropertyName("server_version")] public string? ServerVersion { get; init; }
    [JsonPropertyName("target_server_version")] public string? TargetServerVersion { get; init; }

    /// <summary>Matrix's own reading of the server it is pinned against.</summary>
    [JsonPropertyName("server_compatibility")] public string? ServerCompatibility { get; init; }

    [JsonPropertyName("uptime_seconds")] public long? UptimeSeconds { get; init; }

    /// <summary>Matrix and the server it seals into must agree on the
    /// contract. <c>exact</c> is the only state in which an evidence id
    /// minted here is certain to resolve there — which is what a citation
    /// like <c>[evidence/&lt;id&gt;#r0003]</c> depends on.</summary>
    [JsonIgnore] public bool LockstepOk => ServerCompatibility == "exact";
}

/// <summary>One applied asset.</summary>
public sealed record ApplyOutcome
{
    /// <summary><c>name@version</c>.</summary>
    [JsonPropertyName("asset_ref")] public string AssetRef { get; init; } = "";

    [JsonPropertyName("kind")] public string Kind { get; init; } = "";

    /// <summary>True when this apply changed nothing (a byte-identical
    /// re-apply). Ordinary GitOps, not an error.</summary>
    [JsonPropertyName("unchanged")] public bool Unchanged { get; init; }

    /// <summary>Advisory findings the asset applied in spite of. Three
    /// validator codes are warnings rather than errors, so an apply can
    /// succeed carrying findings, and dropping them would throw away the only
    /// warning an operator gets.</summary>
    [JsonPropertyName("findings")]
    public IReadOnlyList<ValidationFinding> Findings { get; init; } = [];
}

public sealed record ValidationFinding
{
    [JsonPropertyName("code")] public string Code { get; init; } = "";

    /// <summary>A pointer into the asset, e.g. <c>spec.sync.entity</c>.</summary>
    [JsonPropertyName("path")] public string Path { get; init; } = "";

    [JsonPropertyName("message")] public string Message { get; init; } = "";
}

/// <summary>What Matrix's validators said about an asset.</summary>
/// <remarks>
/// <see cref="Valid"/> is carried rather than inferred from an empty
/// <see cref="Findings"/> list, because those are not the same question.
/// Three finding codes — limits.above-inline-seal, mapping.authority-inert
/// and authorization.classes-ignored — are advisory, and an asset producing
/// one is valid and will apply. Deciding validity by counting findings would
/// refuse three healthy assets, and would do it in the client, where nobody
/// would think to look.
/// </remarks>
public sealed record ValidationOutcome
{
    [JsonPropertyName("valid")] public bool Valid { get; init; }

    [JsonPropertyName("findings")]
    public IReadOnlyList<ValidationFinding> Findings { get; init; } = [];
}

public sealed record AssetSummary
{
    [JsonPropertyName("asset_ref")] public string AssetRef { get; init; } = "";
    [JsonPropertyName("name")] public string Name { get; init; } = "";
    [JsonPropertyName("version")] public int Version { get; init; }
    [JsonPropertyName("kind")] public string Kind { get; init; } = "";
    [JsonPropertyName("created_at")] public string CreatedAt { get; init; } = "";
    [JsonPropertyName("source")] public string? Source { get; init; }
}

internal sealed record AssetListResponse
{
    [JsonPropertyName("assets")] public IReadOnlyList<AssetSummary> Assets { get; init; } = [];
}

public sealed record VerifiedQuestion
{
    [JsonPropertyName("question")] public string Question { get; init; } = "";
    [JsonPropertyName("ok")] public bool Ok { get; init; }
    [JsonPropertyName("rows")] public int? Rows { get; init; }

    /// <summary>The identity a seal uses — over the canonical encoding, not
    /// the rendered text.</summary>
    [JsonPropertyName("logical_result_hash")] public string? LogicalResultHash { get; init; }

    [JsonPropertyName("failures")] public IReadOnlyList<string> Failures { get; init; } = [];
}

/// <summary>A contract's or a view's verified questions, run.</summary>
public sealed record VerifyOutcome
{
    [JsonPropertyName("contract")] public string Contract { get; init; } = "";
    [JsonPropertyName("passed")] public int Passed { get; init; }
    [JsonPropertyName("failed")] public int Failed { get; init; }

    /// <summary>Semantic views only: the definition the questions ran under.
    /// A later execute is held to it.</summary>
    [JsonPropertyName("fingerprint")] public string? Fingerprint { get; init; }

    [JsonPropertyName("questions")]
    public IReadOnlyList<VerifiedQuestion> Questions { get; init; } = [];
}

/// <summary>A queued job. The enqueue routes answer with an id rather than an
/// outcome, so a caller has something to watch.</summary>
public sealed record JobAccepted
{
    [JsonPropertyName("accepted")] public int Accepted { get; init; }
    [JsonPropertyName("jobs")] public IReadOnlyList<string> Jobs { get; init; } = [];
    [JsonPropertyName("detail")] public string Detail { get; init; } = "";
}

/// <summary>The two promotion gates as the latest completed run measured
/// them, beside the thresholds in force right now.</summary>
public sealed record PromotionGates
{
    [JsonPropertyName("identity_precision")] public double IdentityPrecision { get; init; }
    [JsonPropertyName("value_conformance")] public double ValueConformance { get; init; }
    [JsonPropertyName("min_identity_precision")] public double MinIdentityPrecision { get; init; }
    [JsonPropertyName("min_value_conformance")] public double MinValueConformance { get; init; }
    [JsonPropertyName("observations")] public long Observations { get; init; }
    [JsonPropertyName("run_id")] public string? RunId { get; init; }
}

/// <summary>Whether a mapping may write canon, and what the numbers say.</summary>
public sealed record PromotionStatus
{
    /// <summary><c>name@version</c> — the service answers with the asset ref,
    /// not the bare name the caller passed.</summary>
    [JsonPropertyName("mapping")] public string Mapping { get; init; } = "";

    /// <summary>shadow | authoritative — what the ASSET declares. The
    /// declaration is the intent and the promotion is the decision; both are
    /// required before a claim reaches the ledger.</summary>
    [JsonPropertyName("mode")] public string Mode { get; init; } = "";

    [JsonPropertyName("promoted")] public bool Promoted { get; init; }
    [JsonPropertyName("promoted_version")] public int? PromotedVersion { get; init; }
    [JsonPropertyName("decision_id")] public string? DecisionId { get; init; }
    [JsonPropertyName("promoted_at")] public string? PromotedAt { get; init; }

    /// <summary>Null until a reconcile pass has completed: the gates measure
    /// a run, and there is nothing honest to report before one exists.</summary>
    [JsonPropertyName("gates")] public PromotionGates? Gates { get; init; }

    [JsonPropertyName("authority_scopes")] public int AuthorityScopes { get; init; }

    /// <summary>The most recent reconcile pass, whatever its state — the
    /// field that answers "did the pass I just queued refuse?". Raw JSON: it
    /// is a run record for reading, and its shape grows.</summary>
    [JsonPropertyName("latest_run")] public JsonElement? LatestRun { get; init; }

    /// <summary>Shorthand for <c>Gates?.IdentityPrecision</c>. Null when no
    /// run has been measured — which is a different fact from a precision of
    /// zero, and the reason this is nullable.</summary>
    [JsonIgnore] public double? IdentityPrecision => Gates?.IdentityPrecision;

    /// <summary>Shorthand for <c>Gates?.ValueConformance</c>.</summary>
    [JsonIgnore] public double? ValueConformance => Gates?.ValueConformance;
}

internal sealed record JournalListResponse
{
    [JsonPropertyName("entries")] public IReadOnlyList<JsonElement> Entries { get; init; } = [];
    [JsonPropertyName("next_before")] public string? NextBefore { get; init; }
}

internal sealed record PromoteBody
{
    [JsonPropertyName("decision_id")] public required string DecisionId { get; init; }
    [JsonPropertyName("actor")] public string? Actor { get; init; }
    [JsonPropertyName("reason")] public string? Reason { get; init; }
}

internal sealed record DecisionBody
{
    [JsonPropertyName("decision_id")] public required string DecisionId { get; init; }
}

/// <summary>Source-generated serialization context (AOT-safe; casing pinned by
/// the JsonPropertyName attributes above).</summary>
[JsonSourceGenerationOptions(DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull)]
[JsonSerializable(typeof(MatrixVersion))]
[JsonSerializable(typeof(ApplyOutcome))]
[JsonSerializable(typeof(ValidationOutcome))]
[JsonSerializable(typeof(AssetListResponse))]
[JsonSerializable(typeof(VerifyOutcome))]
[JsonSerializable(typeof(JobAccepted))]
[JsonSerializable(typeof(PromotionStatus))]
[JsonSerializable(typeof(JournalListResponse))]
[JsonSerializable(typeof(PromoteBody))]
[JsonSerializable(typeof(DecisionBody))]
[JsonSerializable(typeof(JsonElement))]
internal sealed partial class MatrixJsonContext : JsonSerializerContext;
