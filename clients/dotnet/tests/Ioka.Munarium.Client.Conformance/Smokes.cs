// SPDX-License-Identifier: Apache-2.0
// Per-plane smokes beyond the 7 core scenarios (postgres store required):
// streamed content-addressed ingest, retrieval with the ProvenanceEnvelope,
// shape publication, the runbook approve flow, the BYOK provider surface,
// and the documented gRPC transport gaps (honest Unsupported, never silent).

using System.Security.Cryptography;
using System.Text;
using Xunit;

namespace Ioka.Munarium.Client.Conformance;

public class Smokes
{
    private const string ShapeRef = "netsmoke-tickets@1";

    private const string ShapeYaml = """
        apiVersion: munarium.ioka.io/v1
        kind: Shape
        metadata: { name: netsmoke-tickets, version: 1 }
        spec:
          fact:
            schema:
              type: object
              properties:
                subject: { type: string }
                key: { type: string }
                value: { type: string, minLength: 1 }
              required: [subject, key, value]
            supersession:
              identity: [subject, key]
          chunking: { max_chars: 400 }
          indexing: { rrf_k: 60 }
        """;

    private const string RunbookYaml = """
        apiVersion: munarium.ioka.io/v1
        kind: Runbook
        metadata: { name: netsmoke-reindex, version: 1 }
        spec:
          shape: netsmoke-tickets@1
          steps:
            - resolveSources: {}
            - buildIndex: {}
            - verify: {}
            - cutover: { approval: required }
            - retireOld: { keep_versions: 2 }
        """;

    private const string ProviderYaml = """
        apiVersion: munarium.ioka.io/v1
        kind: ProviderConfig
        metadata: { name: netsmoke-anthropic }
        spec:
          provider: anthropic
          models: { complete: [claude-sonnet-4-6] }
          credentialRef: { env: MUNARIUM_SECRET_NETSMOKE_UNSET }
        """;

    /// <summary>A REPLAYABLE two-chunk source — the factory shape the
    /// transports need to retry a transient upload failure.</summary>
    private static ChunkSource TwoChunks(byte[] data)
    {
        var mid = data.Length / 2;
        return Chunks.FromList([data.AsMemory(0, mid), data.AsMemory(mid)]);
    }

    [SkippableFact]
    public async Task PlaneSmokes()
    {
        await using var rest = Clients.Make("rest");
        await using var grpc = Clients.Make("grpc");
        var version = await rest.Commands.CreateVersionAsync();

        // shapes: publication witnessed in the named lineage
        var applied = await rest.Runbooks.ApplyShapeAsync(ShapeYaml, version);
        Assert.Equal(ShapeRef, applied.ShapeRef);
        Assert.False(string.IsNullOrEmpty(applied.EventId));

        // ingest: streamed, content-addressed, idempotent by content
        var content = Encoding.UTF8.GetBytes(
            $"alpha ticket {DateTime.UtcNow.Ticks:x}: the printer prints. " +
            "beta ticket: the printer does not print.");
        var declared = Convert.ToHexStringLower(SHA256.HashData(content));
        var first = await rest.Ingest.PutSourceAsync(
            TwoChunks(content), declared, "text/plain", "netsmoke.txt", ShapeRef);
        Assert.Equal(declared, first.ContentHash);
        Assert.False(first.AlreadyExisted);
        var second = await rest.Ingest.PutSourceAsync(
            TwoChunks(content), declared, filename: "netsmoke.txt", shapeRef: ShapeRef);
        Assert.True(second.AlreadyExisted);

        var recorded = await rest.Ingest.RecordIngestAsync(version, declared, ShapeRef);
        Assert.True(recorded.Seq > 0);

        // retrieval: build + search; the envelope is load-bearing
        var built = await rest.Retrieval.BuildIndexAsync(ShapeRef, version);
        Assert.True(built.Active);
        var result = await rest.Retrieval.SearchAsync("printer", ShapeRef, topK: 5);
        Assert.NotEmpty(result.Hits);
        Assert.False(string.IsNullOrEmpty(result.Envelope.IndexVersion));

        // runbooks: run -> awaiting_approval -> approve -> done
        await rest.Runbooks.ApplyRunbookAsync(RunbookYaml);
        var run = await rest.Runbooks.RunRunbookAsync("netsmoke-reindex", version);
        Assert.Equal("awaiting_approval", run.State);
        var status = await rest.Runbooks.GetRunAsync(run.RunId);
        var awaiting = status.Steps.First(s => s.State == "awaiting_approval");
        var approved = await rest.Runbooks.ApproveStepAsync(run.RunId, awaiting.Ordinal);
        Assert.Equal("done", approved.State);

        // providers: BYOK surface answers (fail-closed creds, no live call)
        Assert.Equal("netsmoke-anthropic", await rest.Providers.ApplyConfigAsync(ProviderYaml));
        try
        {
            _ = await rest.Providers.HealthAsync("netsmoke-anthropic");
        }
        catch (MunariumProviderException)
        {
            // typed fail-closed credential error is the expected demo outcome
        }

        // -- gRPC cross-plane half -----------------------------------------
        Assert.Equal(ShapeRef, (await grpc.Runbooks.ApplyShapeAsync(ShapeYaml)).ShapeRef);

        var content2 = Encoding.UTF8.GetBytes(
            $"gamma ticket {DateTime.UtcNow.Ticks:x}: the scanner scans.");
        var declared2 = Convert.ToHexStringLower(SHA256.HashData(content2));
        var uploaded = await grpc.Ingest.PutSourceAsync(
            TwoChunks(content2), declared2, "text/plain",
            filename: "netsmoke-grpc.txt", shapeRef: ShapeRef);
        Assert.Equal(declared2, uploaded.ContentHash);
        Assert.True((await grpc.Ingest.RecordIngestAsync(version, declared2, ShapeRef)).Seq > 0);

        var grpcResult = await grpc.Retrieval.SearchAsync("printer", ShapeRef, topK: 5);
        Assert.NotEmpty(grpcResult.Hits);
        Assert.False(string.IsNullOrEmpty(grpcResult.Envelope.IndexVersion));

        await Assert.ThrowsAsync<UnsupportedTransportException>(() =>
            grpc.Retrieval.BuildIndexAsync(ShapeRef));

        // cross-plane run visibility — version_id now rides the wire (C5)
        var grpcRun = await grpc.Runbooks.GetRunAsync(run.RunId);
        Assert.Equal("done", grpcRun.State);
        Assert.Equal(version, grpcRun.VersionId);

        // invocation provenance is accepted on gRPC (C5): the fail-closed
        // credential error proves the call reached the provider gateway
        // rather than being rejected as an unsupported transport.
        await Assert.ThrowsAsync<MunariumProviderException>(() =>
            grpc.Providers.CompleteAsync("netsmoke-anthropic", "hi", versionId: version));
    }
}
