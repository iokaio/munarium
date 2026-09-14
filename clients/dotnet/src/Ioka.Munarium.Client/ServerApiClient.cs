// SPDX-License-Identifier: Apache-2.0
using System.Runtime.CompilerServices;
using System.Text.Json;
using Google.Protobuf;
using Grpc.Core;
using Grpc.Net.Client;

namespace Ioka.Munarium.Client;

/// <summary>Lossless request for a named Server API operation. Path/query values are unescaped.</summary>
public sealed record ApiRequest
{
    public IReadOnlyDictionary<string, string> Path { get; init; } = new Dictionary<string, string>();
    public IReadOnlyList<KeyValuePair<string, string>> Query { get; init; } = [];
    public byte[] Body { get; init; } = [];
    public string ContentType { get; init; } = "";
    public IReadOnlyDictionary<string, string> SourceHeaders { get; init; } = new Dictionary<string, string>();
    public string? IdempotencyKey { get; init; }
    public static ApiRequest Json<T>(T value, IReadOnlyDictionary<string, string>? path = null) =>
        new() { Body = JsonSerializer.SerializeToUtf8Bytes(value), ContentType = "application/json", Path = path ?? new Dictionary<string, string>() };
}

/// <summary>JSON, YAML or binary response; streaming methods return incremental body fragments.</summary>
public sealed record ApiResponse(int Status, string ContentType, byte[] Body)
{
    public JsonElement Json() { using var doc = JsonDocument.Parse(Body); return doc.RootElement.Clone(); }
}

/// <summary>Complete named API over REST or gRPC. Calls send once; writes and streams are never replayed.</summary>
public sealed partial class ServerApiClient : IAsyncDisposable
{
    private const int MaxBytes = 256 * 1024 * 1024;
    private readonly MunariumClientOptions _options;
    private readonly HttpClient? _http;
    private readonly GrpcChannel? _channel;
    private ServerApiClient(MunariumClientOptions options, bool grpc)
    {
        _options = options;
        if (grpc) _channel = GrpcChannel.ForAddress(options.Endpoint, new GrpcChannelOptions { MaxReceiveMessageSize = MaxBytes + 65536, MaxSendMessageSize = MaxBytes + 65536 });
        else _http = new HttpClient(new SocketsHttpHandler { ConnectTimeout = options.ConnectTimeout, AllowAutoRedirect = false }) { Timeout = Timeout.InfiniteTimeSpan };
    }
    public static ServerApiClient Rest(MunariumClientOptions options) => new(options, false);
    public static ServerApiClient Grpc(MunariumClientOptions options) => new(options, true);
    public ValueTask DisposeAsync() { _http?.Dispose(); _channel?.Dispose(); return ValueTask.CompletedTask; }

    private static string UriFor(string template, ApiRequest request)
    {
        foreach (var (key, value) in request.Path)
        {
            var marker = "{" + key + "}";
            if (!template.Contains(marker, StringComparison.Ordinal) || value is "" or "." or "..") throw new InvalidInputException("invalid path parameter");
            template = template.Replace(marker, Uri.EscapeDataString(value), StringComparison.Ordinal);
        }
        if (template.Contains('{')) throw new InvalidInputException("missing path parameter");
        return template + (request.Query.Count == 0 ? "" : "?" + string.Join("&", request.Query.Select(p => Uri.EscapeDataString(p.Key) + "=" + Uri.EscapeDataString(p.Value))));
    }
    private static Method<Mmp.V1.ServerApiRequest, Mmp.V1.ServerApiResponse> Rpc(string name, MethodType type) =>
        new(type, "mmp.v1.ServerApiService", name,
            Marshallers.Create((Mmp.V1.ServerApiRequest value) => value.ToByteArray(), bytes => Mmp.V1.ServerApiRequest.Parser.ParseFrom(bytes)),
            Marshallers.Create((Mmp.V1.ServerApiResponse value) => value.ToByteArray(), bytes => Mmp.V1.ServerApiResponse.Parser.ParseFrom(bytes)));
    private CallOptions Options(ApiRequest request, string method, CancellationToken ct)
    {
        var headers = new Metadata();
        if (_options.Token != null) headers.Add("authorization", "Bearer " + _options.Token);
        if (_options.Uid != null) headers.Add("munarium-uid", _options.Uid);
        if (request.IdempotencyKey != null || method != "GET") headers.Add("idempotency-key", request.IdempotencyKey ?? Guid.NewGuid().ToString());
        return new(headers, method == "GET" ? DateTime.UtcNow + _options.RequestTimeout : null, ct);
    }
    private static Mmp.V1.ServerApiRequest Proto(ApiRequest request)
    {
        if (request.Body.Length > MaxBytes) throw new InvalidInputException("request exceeds payload budget");
        var result = new Mmp.V1.ServerApiRequest { Body = ByteString.CopyFrom(request.Body), ContentType = request.ContentType };
        foreach (var (key, value) in request.Path) result.PathParameters.Add(key, value);
        foreach (var (key, value) in request.Query) result.QueryParameters.Add(new Mmp.V1.ServerApiParameter { Name = key, Value = value });
        foreach (var (key, value) in request.SourceHeaders) result.SourceHeaders.Add(key, value);
        return result;
    }
    private HttpRequestMessage HttpRequest(string method, string path, string media, ApiRequest input)
    {
        var request = new HttpRequestMessage(new HttpMethod(method), _options.Endpoint.TrimEnd('/') + UriFor(path, input)) { Content = new ByteArrayContent(input.Body) };
        request.Content.Headers.ContentType = System.Net.Http.Headers.MediaTypeHeaderValue.Parse(input.ContentType.Length == 0 ? media : input.ContentType);
        foreach (var (key, value) in input.SourceHeaders)
        {
            if (key is not ("x-filename" or "x-content-sha256" or "x-shape-ref")) throw new InvalidInputException("unsupported source header");
            request.Headers.Add(key, value);
        }
        if (_options.Token != null) request.Headers.Authorization = new("Bearer", _options.Token);
        if (_options.Uid != null) request.Headers.Add("X-Munarium-Uid", _options.Uid);
        if (input.IdempotencyKey != null || method != "GET") request.Headers.Add("Idempotency-Key", input.IdempotencyKey ?? Guid.NewGuid().ToString());
        return request;
    }
    private static async Task<byte[]> ReadBounded(HttpContent content, CancellationToken ct)
    {
        await using var source = await content.ReadAsStreamAsync(ct).ConfigureAwait(false);
        using var result = new MemoryStream(); var buffer = new byte[65536];
        while (true) { var count = await source.ReadAsync(buffer, ct).ConfigureAwait(false); if (count == 0) break;
            if (result.Length + count > MaxBytes) throw new MunariumTransportException("response exceeds payload budget");
            result.Write(buffer, 0, count); }
        return result.ToArray();
    }
    private async Task<ApiResponse> CallAsync(string rpc, string method, string path, string media, ApiRequest input, CancellationToken ct)
    {
        if (input.Body.Length > MaxBytes) throw new InvalidInputException("request exceeds payload budget");
        if (_channel != null)
        {
            try { using var call = _channel.CreateCallInvoker().AsyncUnaryCall(Rpc(rpc, MethodType.Unary), null, Options(input, method, ct), Proto(input));
                var response = await call.ResponseAsync.ConfigureAwait(false); return new((int)response.Status, response.ContentType, response.Body.ToByteArray()); }
            catch (RpcException error) { throw Errors.FromRpc(error); }
        }
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(ct);
        if (method == "GET") deadline.CancelAfter(_options.RequestTimeout);
        try { using var request = HttpRequest(method, path, media, input);
            using var response = await _http!.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, deadline.Token).ConfigureAwait(false);
            var body = await ReadBounded(response.Content, deadline.Token).ConfigureAwait(false);
            if (!response.IsSuccessStatusCode) throw Errors.FromProblem((int)response.StatusCode, System.Text.Encoding.UTF8.GetString(body), response.Headers.RetryAfter?.Delta);
            return new((int)response.StatusCode, response.Content.Headers.ContentType?.ToString() ?? "", body); }
        catch (HttpRequestException error) { throw new MunariumTransportException(error.Message); }
    }
    private static async Task<bool> Next(IAsyncStreamReader<Mmp.V1.ServerApiResponse> stream, CancellationToken ct)
    { try { return await stream.MoveNext(ct).ConfigureAwait(false); } catch (RpcException error) { throw Errors.FromRpc(error); } }
    private async IAsyncEnumerable<ApiResponse> StreamAsync(string rpc, string method, string path, string media, ApiRequest input, [EnumeratorCancellation] CancellationToken ct)
    {
        if (input.Body.Length > MaxBytes) throw new InvalidInputException("request exceeds payload budget");
        if (_channel != null)
        {
            using var call = _channel.CreateCallInvoker().AsyncServerStreamingCall(Rpc(rpc, MethodType.ServerStreaming), null, Options(input, method, ct), Proto(input));
            while (await Next(call.ResponseStream, ct).ConfigureAwait(false)) { var part = call.ResponseStream.Current; yield return new((int)part.Status, part.ContentType, part.Body.ToByteArray()); }
            yield break;
        }
        using var request = HttpRequest(method, path, media, input);
        using var response = await _http!.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, ct).ConfigureAwait(false);
        if (!response.IsSuccessStatusCode) throw Errors.FromProblem((int)response.StatusCode, System.Text.Encoding.UTF8.GetString(await ReadBounded(response.Content, ct).ConfigureAwait(false)), response.Headers.RetryAfter?.Delta);
        await using var stream = await response.Content.ReadAsStreamAsync(ct).ConfigureAwait(false);
        var buffer = new byte[65536];
        while (true) { var count = await stream.ReadAsync(buffer, ct).ConfigureAwait(false); if (count == 0) yield break;
            yield return new((int)response.StatusCode, response.Content.Headers.ContentType?.ToString() ?? "", buffer[..count]); }
    }
}
