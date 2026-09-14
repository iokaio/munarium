// SPDX-License-Identifier: Apache-2.0
using System.Text;
using Xunit;

namespace Ioka.Munarium.Client.Conformance;

public class ServerApiTests
{
    [SkippableTheory]
    [InlineData(false)]
    [InlineData(true)]
    public async Task VocabularyAndSourceMetadataAcrossTransports(bool grpc)
    {
        var endpoint = Environment.GetEnvironmentVariable(grpc ? "MUNARIUM_GRPC_URL" : "MUNARIUM_REST_URL");
        Skip.If(endpoint == null, "live Server endpoint unset");
        var options = new MunariumClientOptions { Endpoint = endpoint!, Token = Environment.GetEnvironmentVariable("MUNARIUM_TOKEN") ?? "devtoken", Uid = "api-dotnet-conformance" };
        await using var api = grpc ? ServerApiClient.Grpc(options) : ServerApiClient.Rest(options);
        Assert.Equal(MunariumClient.TargetServerVersion, (await api.VersionInfoAsync()).Json().GetProperty("version").GetString());
        var shape = "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: {name: api-dotnet-docs, version: 1}\nspec:\n  fact:\n    schema: {type: object}\n";
        await api.ApplyShapeAsync(new ApiRequest { Body = Encoding.UTF8.GetBytes(shape), ContentType = "text/yaml" });
        var name = "api-dotnet-" + Guid.NewGuid().ToString("N");
        var collection = (await api.CreateCollectionAsync(ApiRequest.Json(new { name, shape_ref = "api-dotnet-docs@1", access_level = 0, compartments = Array.Empty<string>() }))).Json();
        var path = new Dictionary<string, string> { ["id"] = collection.GetProperty("id").GetString()! };
        var initial = (await api.GetCollectionVocabularyAsync(new ApiRequest { Path = path })).Json();
        var body = ApiRequest.Json(new { revision = initial.GetProperty("revision").GetInt64(), enabled = true, auto_generate = false, sampling = (object?)null, groups = new[] { new[] { "purchase order", "procurement request" } } }, path);
        var saved = (await api.ReplaceCollectionVocabularyAsync(body)).Json();
        Assert.False(saved.GetProperty("auto_generate").GetBoolean());
        Assert.Equal(System.Text.Json.JsonValueKind.Null, saved.GetProperty("sampling").ValueKind);
        await Assert.ThrowsAsync<InvalidInputException>(() => api.ReplaceCollectionVocabularyAsync(body));
        var disabled = (await api.UpdateCollectionVocabularyAsync(ApiRequest.Json(new { revision = saved.GetProperty("revision").GetInt64(), enabled = false }, path))).Json();
        Assert.False(disabled.GetProperty("enabled").GetBoolean());
        Assert.Equal(disabled.GetProperty("revision").GetInt64(), (await api.GetVocabularyRevisionAsync(new ApiRequest { Path = path })).Json().GetProperty("revision").GetInt64());
        var bytes = Encoding.UTF8.GetBytes("A purchase order needs supervisor approval.");
        var filename = name + "/ordering.txt";
        var source = (await api.PutSourceAsync(new ApiRequest { Body = bytes, ContentType = "text/plain", SourceHeaders = new Dictionary<string, string> { ["x-filename"] = filename } })).Json();
        var metadata = (await api.GetSourceAsync(new ApiRequest { Path = new Dictionary<string, string> { ["source_id"] = source.GetProperty("source_id").GetString()! } })).Json();
        Assert.Equal(filename, metadata.GetProperty("filename").GetString());
        await Assert.ThrowsAsync<MunariumNotFoundException>(() => api.GetSourceAsync(new ApiRequest { Path = new Dictionary<string, string> { ["source_id"] = "src-absent" } }));
    }
}
