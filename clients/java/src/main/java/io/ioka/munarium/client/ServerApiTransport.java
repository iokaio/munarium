// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.google.protobuf.ByteString;
import io.grpc.CallOptions;
import io.grpc.Channel;
import io.grpc.ClientInterceptors;
import io.grpc.Context;
import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;
import io.grpc.Metadata;
import io.grpc.MethodDescriptor;
import io.grpc.StatusRuntimeException;
import io.grpc.protobuf.ProtoUtils;
import io.grpc.stub.ClientCalls;
import io.grpc.stub.MetadataUtils;
import io.ioka.munarium.client.errors.InvalidInputException;
import io.ioka.munarium.client.errors.MunariumTransportException;
import io.ioka.munarium.client.errors.Problems;
import io.ioka.munarium.client.grpc.GrpcTransport;
import java.io.IOException;
import java.io.InputStream;
import java.net.URI;
import java.net.URLEncoder;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.function.Consumer;
import java.util.function.Supplier;
import mmp.v1.ServerApi;

/** Shared complete-API transport. Calls send once; no write or stream is replayed. */
public class ServerApiTransport implements AutoCloseable {
    private static final int MAX_BYTES = 256 * 1024 * 1024;
    private static final ObjectMapper JSON = new ObjectMapper();
    public record ApiRequest(Map<String, String> path, List<Map.Entry<String, String>> query,
            byte[] body, String contentType, Map<String, String> sourceHeaders, String idempotencyKey) {
        public ApiRequest { path = Map.copyOf(path); query = List.copyOf(query); body = body.clone(); sourceHeaders = Map.copyOf(sourceHeaders); }
        @Override public byte[] body() { return body.clone(); }
        public static ApiRequest empty() { return new ApiRequest(Map.of(), List.of(), new byte[0], "", Map.of(), null); }
        public static ApiRequest json(JsonNode value, Map<String, String> path) {
            try { return new ApiRequest(path, List.of(), JSON.writeValueAsBytes(value), "application/json", Map.of(), null); }
            catch (IOException error) { throw new InvalidInputException("invalid JSON request"); }
        }
        public ApiRequest withPath(String name, String value) { var copy = new java.util.HashMap<>(path); copy.put(name, value); return new ApiRequest(copy, query, body, contentType, sourceHeaders, idempotencyKey); }
    }
    public record ApiResponse(int status, String contentType, byte[] body) {
        public ApiResponse { body = body.clone(); }
        @Override public byte[] body() { return body.clone(); }
        public JsonNode json() { try { return JSON.readTree(body); } catch (IOException error) { throw new InvalidInputException("invalid JSON response"); } }
    }
    private final MunariumClientOptions options;
    private final ManagedChannel channel;
    private final HttpClient http;
    private final ExecutorService executor = Executors.newVirtualThreadPerTaskExecutor();
    protected ServerApiTransport(MunariumClientOptions options, boolean grpc) {
        this.options = options;
        if (grpc) {
            URI uri = URI.create(options.endpoint());
            if (uri.getHost() == null || !(uri.getScheme().equals("http") || uri.getScheme().equals("https"))) throw new InvalidInputException("gRPC endpoint must use http or https");
            var builder = ManagedChannelBuilder.forAddress(uri.getHost(), uri.getPort() < 0 ? (uri.getScheme().equals("https") ? 443 : 50051) : uri.getPort()).maxInboundMessageSize(MAX_BYTES + 65536);
            if (uri.getScheme().equals("https")) builder.useTransportSecurity(); else builder.usePlaintext();
            channel = builder.build(); http = null;
        } else { channel = null; http = HttpClient.newBuilder().connectTimeout(options.connectTimeout()).followRedirects(HttpClient.Redirect.NEVER).build(); }
    }
    protected <T> CompletableFuture<T> async(Supplier<T> task) { return CompletableFuture.supplyAsync(task, executor); }
    @Override public void close() { if (channel != null) channel.shutdownNow(); if (http != null) http.shutdownNow(); executor.shutdownNow(); }
    private static String component(String value) { return URLEncoder.encode(value, StandardCharsets.UTF_8).replace("+", "%20").replace("%7E", "~").replace("*", "%2A"); }
    private static String uri(String template, ApiRequest input) {
        for (var entry : input.path.entrySet()) {
            String marker = "{" + entry.getKey() + "}", value = entry.getValue();
            if (!template.contains(marker) || value.isEmpty() || value.equals(".") || value.equals("..")) throw new InvalidInputException("invalid path parameter");
            template = template.replace(marker, component(value));
        }
        if (template.contains("{")) throw new InvalidInputException("missing path parameter");
        var result = new StringBuilder(template);
        for (int i = 0; i < input.query.size(); i++) { var pair = input.query.get(i); result.append(i == 0 ? '?' : '&').append(component(pair.getKey())).append('=').append(component(pair.getValue())); }
        return result.toString();
    }
    private Channel authenticated(ApiRequest input, String method) {
        var metadata = new Metadata();
        if (options.token() != null) metadata.put(Metadata.Key.of("authorization", Metadata.ASCII_STRING_MARSHALLER), "Bearer " + options.token());
        if (options.uid() != null) metadata.put(Metadata.Key.of("munarium-uid", Metadata.ASCII_STRING_MARSHALLER), options.uid());
        if (input.idempotencyKey != null || !method.equals("GET")) metadata.put(Metadata.Key.of("idempotency-key", Metadata.ASCII_STRING_MARSHALLER), input.idempotencyKey == null ? UUID.randomUUID().toString() : input.idempotencyKey);
        return ClientInterceptors.intercept(channel, MetadataUtils.newAttachHeadersInterceptor(metadata));
    }
    private static ServerApi.ServerApiRequest proto(ApiRequest input) {
        if (input.body.length > MAX_BYTES) throw new InvalidInputException("request exceeds payload budget");
        var result = ServerApi.ServerApiRequest.newBuilder().putAllPathParameters(input.path).setBody(ByteString.copyFrom(input.body)).setContentType(input.contentType).putAllSourceHeaders(input.sourceHeaders);
        for (var pair : input.query) result.addQueryParameters(ServerApi.ServerApiParameter.newBuilder().setName(pair.getKey()).setValue(pair.getValue()));
        return result.build();
    }
    private static MethodDescriptor<ServerApi.ServerApiRequest, ServerApi.ServerApiResponse> rpc(String name, boolean streaming) {
        return MethodDescriptor.<ServerApi.ServerApiRequest, ServerApi.ServerApiResponse>newBuilder()
            .setType(streaming ? MethodDescriptor.MethodType.SERVER_STREAMING : MethodDescriptor.MethodType.UNARY)
            .setFullMethodName("mmp.v1.ServerApiService/" + name)
            .setRequestMarshaller(ProtoUtils.marshaller(ServerApi.ServerApiRequest.getDefaultInstance()))
            .setResponseMarshaller(ProtoUtils.marshaller(ServerApi.ServerApiResponse.getDefaultInstance())).build();
    }
    private HttpResponse<InputStream> http(String method, String path, String media, ApiRequest input) throws IOException, InterruptedException {
        var request = HttpRequest.newBuilder(URI.create(options.endpoint().replaceAll("/$", "") + uri(path, input)))
            .method(method, HttpRequest.BodyPublishers.ofByteArray(input.body)).header("content-type", input.contentType.isEmpty() ? media : input.contentType);
        if (options.token() != null) request.header("authorization", "Bearer " + options.token());
        if (options.uid() != null) request.header("x-munarium-uid", options.uid());
        if (input.idempotencyKey != null || !method.equals("GET")) request.header("idempotency-key", input.idempotencyKey == null ? UUID.randomUUID().toString() : input.idempotencyKey);
        for (var entry : input.sourceHeaders.entrySet()) {
            if (!List.of("x-filename", "x-content-sha256", "x-shape-ref").contains(entry.getKey())) throw new InvalidInputException("unsupported source header");
            request.header(entry.getKey(), entry.getValue());
        }
        if (method.equals("GET")) request.timeout(options.requestTimeout());
        var response = http.send(request.build(), HttpResponse.BodyHandlers.ofInputStream());
        if (response.statusCode() >= 400) { try (var stream = response.body()) { throw Problems.fromProblemJson(response.statusCode(), JSON.readTree(readBounded(stream)), null); } }
        return response;
    }
    private static byte[] readBounded(InputStream stream) throws IOException {
        byte[] bytes = stream.readNBytes(MAX_BYTES + 1);
        if (bytes.length > MAX_BYTES) throw new MunariumTransportException("response exceeds payload budget", true);
        return bytes;
    }
    protected ApiResponse call(String name, String method, String path, String media, ApiRequest request) {
        ApiRequest input = request == null ? ApiRequest.empty() : request;
        if (input.body.length > MAX_BYTES) throw new InvalidInputException("request exceeds payload budget");
        if (channel != null) {
            try { var opts = method.equals("GET") ? CallOptions.DEFAULT.withDeadlineAfter(options.requestTimeout().toMillis(), TimeUnit.MILLISECONDS) : CallOptions.DEFAULT;
                var response = ClientCalls.blockingUnaryCall(authenticated(input, method), rpc(name, false), opts, proto(input));
                return new ApiResponse(response.getStatus(), response.getContentType(), response.getBody().toByteArray()); }
            catch (StatusRuntimeException error) { throw GrpcTransport.decode(error); }
        }
        try { var response = http(method, path, media, input); try (var body = response.body()) { return new ApiResponse(response.statusCode(), response.headers().firstValue("content-type").orElse(""), readBounded(body)); } }
        catch (InterruptedException error) { Thread.currentThread().interrupt(); throw new MunariumTransportException("request interrupted", true); }
        catch (IOException error) { throw new MunariumTransportException(error.getMessage(), true); }
    }
    protected void stream(String name, String method, String path, String media, ApiRequest request, Consumer<ApiResponse> onPart) {
        ApiRequest input = request == null ? ApiRequest.empty() : request;
        if (input.body.length > MAX_BYTES) throw new InvalidInputException("request exceeds payload budget");
        if (channel != null) {
            try (var context = Context.current().withCancellation()) {
                var previous = context.attach();
                try { var parts = ClientCalls.blockingServerStreamingCall(authenticated(input, method), rpc(name, true), CallOptions.DEFAULT, proto(input));
                    parts.forEachRemaining(part -> onPart.accept(new ApiResponse(part.getStatus(), part.getContentType(), part.getBody().toByteArray()))); }
                catch (StatusRuntimeException error) { throw GrpcTransport.decode(error); }
                finally { context.detach(previous); }
            }
            return;
        }
        try { var response = http(method, path, media, input); try (var body = response.body()) {
            byte[] buffer = new byte[65536]; int count;
            while ((count = body.read(buffer)) >= 0) { if (count != 0) onPart.accept(new ApiResponse(response.statusCode(), response.headers().firstValue("content-type").orElse(""), java.util.Arrays.copyOf(buffer, count))); }
        } }
        catch (InterruptedException error) { Thread.currentThread().interrupt(); throw new MunariumTransportException("stream interrupted", true); }
        catch (IOException error) { throw new MunariumTransportException(error.getMessage(), true); }
    }
}
