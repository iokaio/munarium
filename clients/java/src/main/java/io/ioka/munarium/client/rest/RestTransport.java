// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.rest;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.JsonNode;
import io.ioka.munarium.client.MunariumClientOptions;
import io.ioka.munarium.client.Transport;
import io.ioka.munarium.client.errors.InvalidInputException;
import io.ioka.munarium.client.errors.MunariumException;
import io.ioka.munarium.client.errors.MunariumTransportException;
import io.ioka.munarium.client.errors.Problems;
import io.ioka.munarium.client.errors.OverloadedException;
import io.ioka.munarium.client.errors.UnexpectedServerException;
import io.ioka.munarium.client.internal.Wire;
import io.ioka.munarium.client.model.Authoring;
import io.ioka.munarium.client.model.Evidence;
import io.ioka.munarium.client.model.Ingesting;
import io.ioka.munarium.client.model.Json;
import io.ioka.munarium.client.model.Ledger;
import io.ioka.munarium.client.model.Memory;
import io.ioka.munarium.client.model.Meta;
import io.ioka.munarium.client.model.Providers;
import io.ioka.munarium.client.model.Reports;
import io.ioka.munarium.client.model.Retrieval;
import io.ioka.munarium.client.model.Runbooks;
import io.ioka.munarium.client.model.SessionsApi;
import io.ioka.munarium.client.model.Tokens;
import io.ioka.munarium.client.planes.Params;
import io.ioka.munarium.client.planes.Planes;
import java.io.FilterInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.PipedInputStream;
import java.io.PipedOutputStream;
import java.net.ConnectException;
import java.net.UnknownHostException;
import java.net.URI;
import java.net.URLEncoder;
import java.net.http.HttpClient;
import java.net.http.HttpConnectTimeoutException;
import java.net.http.HttpRequest;
import java.net.http.HttpRequest.BodyPublisher;
import java.net.http.HttpRequest.BodyPublishers;
import java.net.http.HttpResponse;
import java.net.http.HttpResponse.BodyHandlers;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.time.ZonedDateTime;
import java.time.format.DateTimeFormatter;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicLong;
import java.util.concurrent.atomic.AtomicReference;
import javax.net.ssl.SSLHandshakeException;
import java.util.function.Consumer;
import java.util.function.Supplier;

/**
 * REST transport over {@code java.net.http}: problem+json error decoding
 * through the one {@link Problems} path, automatic idempotency keys on
 * commands, bounded retries by request class:
 *
 * <ul>
 *   <li>reads (+search): transport failures and transient server outcomes
 *       (overload / 5xx gateway) retried with backoff;
 *   <li>core commands: re-sent with the SAME idempotency key ONLY when the
 *       request provably never reached the server (connect-phase failure)
 *       or the server shed it before executing — the server records an
 *       idempotency key AFTER a command completes, so a possibly-delivered
 *       command is never re-sent (it could execute twice);
 *   <li>non-replayable writes (turns, provider calls, ingest, …): sent
 *       exactly once.
 * </ul>
 *
 * <p>Timeout posture: the per-request deadline applies everywhere EXCEPT
 * the paid/large sends where a 30 s cap is a trap — streamed source upload,
 * the file/bulk ingest bodies (up to 256 MiB), and unary session turns
 * (aborting client-side does not stop the server's paid completion). The
 * SSE turn stream has no overall deadline but a 60 s idle watchdog: the
 * server heartbeats keep-alive comments every 15 s, so a silent wire means
 * a wedged peer.
 */
public final class RestTransport implements Transport {

    private static final Duration SSE_IDLE_TIMEOUT = Duration.ofSeconds(60);

    private enum RetryClass {
        READ,
        COMMAND,
        WRITE_ONCE
    }

    private final HttpClient http;
    private final String base;
    private final Duration requestTimeout;
    private final int readRetries;
    private final String token;
    private final String uid;
    /** Daemon scheduler backing the SSE idle watchdog. */
    private final ScheduledExecutorService watchdog;

    public RestTransport(MunariumClientOptions options) {
        this.http = HttpClient.newBuilder().connectTimeout(options.connectTimeout()).build();
        this.base = options.endpoint().replaceAll("/+$", "");
        this.requestTimeout = options.requestTimeout();
        this.readRetries = options.readRetries();
        this.token = options.token();
        this.uid = options.uid();
        this.watchdog = Executors.newSingleThreadScheduledExecutor(r -> {
            Thread t = new Thread(r, "munarium-sse-watchdog");
            t.setDaemon(true);
            return t;
        });
    }

    @Override
    public void close() {
        watchdog.shutdownNow();
        // Java 21 HttpClient is AutoCloseable: without this, each transport
        // leaks a selector-manager thread + pooled sockets until GC.
        http.close();
    }

    // -- request plumbing ---------------------------------------------------

    private static String seg(String s) {
        // Percent-encode a path segment — promise keys, shape refs, and
        // runbook names are free-form; a raw '/' or '?' must not change the
        // route shape. (URLEncoder is form-encoding: fix the space.)
        return URLEncoder.encode(s, StandardCharsets.UTF_8).replace("+", "%20");
    }

    private URI uri(String path, Map<String, String> params) {
        StringBuilder sb = new StringBuilder(base).append(path);
        if (params != null && !params.isEmpty()) {
            sb.append('?');
            boolean first = true;
            for (var e : params.entrySet()) {
                if (!first) {
                    sb.append('&');
                }
                first = false;
                sb.append(URLEncoder.encode(e.getKey(), StandardCharsets.UTF_8))
                        .append('=')
                        .append(URLEncoder.encode(e.getValue(), StandardCharsets.UTF_8));
            }
        }
        return URI.create(sb.toString());
    }

    private HttpRequest.Builder request(String path, Map<String, String> params, boolean exempt) {
        HttpRequest.Builder b = HttpRequest.newBuilder(uri(path, params));
        if (!exempt) {
            b.timeout(requestTimeout);
        }
        if (token != null) {
            b.header("authorization", "Bearer " + token);
        }
        if (uid != null) {
            b.header("x-munarium-uid", uid);
        }
        return b;
    }

    private static byte[] jsonBody(Object body) {
        try {
            return Json.MAPPER.writeValueAsBytes(body);
        } catch (IOException e) {
            throw new InvalidInputException("unserializable request body: " + e.getMessage());
        }
    }

    /**
     * Stream a JSON value through Jackson's bounded generator buffer instead
     * of materializing a second request-sized byte array. File and bulk
     * requests can approach 256 MiB and already hold base64 content in their
     * DTOs; buffering the full encoded JSON again needlessly doubles their
     * live memory. The pipe provides backpressure and a virtual thread keeps
     * serialization off HttpClient's selector threads.
     */
    static BodyPublisher streamingJsonBody(Object body) {
        return BodyPublishers.ofInputStream(() -> {
            try {
                var failure = new AtomicReference<IOException>();
                var input = new PipedInputStream(64 * 1024);
                var output = new PipedOutputStream(input);
                Thread.ofVirtual().name("munarium-json-body").start(() -> {
                    try (output) {
                        Json.MAPPER.writeValue(output, body);
                    } catch (IOException e) {
                        failure.set(e);
                        try {
                            output.close();
                        } catch (IOException ignored) {
                            // The reader may already have closed the pipe.
                        }
                    }
                });
                return new FilterInputStream(input) {
                    private int checked(int n) throws IOException {
                        if (n < 0 && failure.get() != null) {
                            throw failure.get();
                        }
                        return n;
                    }

                    @Override
                    public int read() throws IOException {
                        return checked(super.read());
                    }

                    @Override
                    public int read(byte[] bytes, int offset, int length) throws IOException {
                        return checked(super.read(bytes, offset, length));
                    }
                };
            } catch (IOException e) {
                throw new InvalidInputException(
                        "cannot open streaming JSON body: " + e.getMessage());
            }
        });
    }

    /** {@code Retry-After} is delta-seconds or an HTTP-date; both → delay. */
    private static Duration retryAfter(HttpResponse<?> resp) {
        return resp.headers().firstValue("retry-after").map(raw -> {
            try {
                return Duration.ofSeconds(Long.parseLong(raw.trim()));
            } catch (NumberFormatException e) {
                try {
                    var when = ZonedDateTime.parse(raw.trim(), DateTimeFormatter.RFC_1123_DATE_TIME);
                    var d = Duration.between(ZonedDateTime.now(when.getZone()), when);
                    return d.isNegative() ? Duration.ZERO : d;
                } catch (RuntimeException e2) {
                    return null;
                }
            }
        }).orElse(null);
    }

    /** The ONE non-success decoder — problem+json through the slug
     * registry with Retry-After preserved. Every consumer of an error
     * response (unary decode, text reads, the SSE pre-stream refusal) goes
     * through here so none can drift. */
    private static MunariumException decodeError(HttpResponse<byte[]> resp) {
        return decodeError(resp.statusCode(), resp.body(), retryAfter(resp));
    }

    private static MunariumException decodeError(int status, byte[] body, Duration retryAfter) {
        JsonNode node;
        try {
            node = Json.MAPPER.readTree(body);
        } catch (IOException e) {
            return new UnexpectedServerException("non-JSON error body (HTTP " + status + ")", status);
        }
        return Problems.fromProblemJson(status, node, retryAfter);
    }

    private static JsonNode decode(HttpResponse<byte[]> resp) {
        if (resp.statusCode() >= 200 && resp.statusCode() < 300) {
            try {
                return Json.MAPPER.readTree(resp.body());
            } catch (IOException e) {
                throw new UnexpectedServerException(
                        "undecodable success body: " + e.getMessage(), resp.statusCode());
            }
        }
        throw decodeError(resp);
    }

    private static boolean delivered(Exception e) {
        // False only for connect-PHASE failures — the request provably never
        // left. TLS handshake and DNS resolution are part of that phase
        // (sibling parity: httpx ConnectError / reqwest is_connect / .NET
        // SecureConnectionError all classify them undelivered).
        return !(e instanceof ConnectException
                || e instanceof HttpConnectTimeoutException
                || e instanceof SSLHandshakeException
                || e instanceof UnknownHostException);
    }

    private JsonNode run(RetryClass cls, HttpRequest.Builder builder, String method, byte[] body,
            String contentType, String idempotencyKey) {
        Supplier<BodyPublisher> publisher = body == null
                ? BodyPublishers::noBody
                : () -> BodyPublishers.ofByteArray(body);
        return runPublisher(cls, builder, method, publisher, contentType, idempotencyKey);
    }

    private JsonNode runPublisher(RetryClass cls, HttpRequest.Builder builder, String method,
            Supplier<BodyPublisher> publisher, String contentType, String idempotencyKey) {
        String idem = cls == RetryClass.COMMAND
                ? (idempotencyKey != null ? idempotencyKey : UUID.randomUUID().toString())
                : null;
        int attempt = 0;
        while (true) {
            attempt++;
            HttpRequest.Builder b = builder.copy();
            if (contentType != null) {
                b.header("content-type", contentType);
            }
            if (idem != null) {
                b.header("idempotency-key", idem);
            }
            b.method(method, publisher.get());
            try {
                return decode(http.send(b.build(), BodyHandlers.ofByteArray()));
            } catch (IOException e) {
                boolean retryable = switch (cls) {
                    case READ -> true;
                    case COMMAND -> !delivered(e);
                    case WRITE_ONCE -> false;
                };
                if (retryable && attempt <= readRetries) {
                    Wire.sleepBackoff(attempt);
                    continue;
                }
                throw new MunariumTransportException(e.toString(), delivered(e));
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw new MunariumTransportException("interrupted", true);
            } catch (MunariumException e) {
                // Reads retry any typed transient. Commands retry ONLY the
                // typed `overloaded` — the server provably shed the request
                // BEFORE executing. A transient 502/504 from a gateway means
                // the command may still be executing upstream, so re-sending
                // could execute it twice (a shared sibling bug found in the
                // C10 review and fixed across Python/.NET/Java; Rust always
                // had it right).
                boolean retryable = switch (cls) {
                    case READ -> e.isTransient();
                    case COMMAND -> e instanceof OverloadedException;
                    case WRITE_ONCE -> false;
                };
                if (retryable && attempt <= readRetries) {
                    Wire.sleepBackoff(attempt);
                    continue;
                }
                throw e;
            }
        }
    }

    /** Deadline-exempt, send-once JSON path for the 256 MiB file/bulk API. */
    private JsonNode runLargeJson(HttpRequest.Builder builder, Object body) {
        return runPublisher(RetryClass.WRITE_ONCE, builder, "POST",
                () -> streamingJsonBody(body), "application/json", null);
    }

    private <T> T get(String path, Map<String, String> params, Class<T> type) {
        return convert(run(RetryClass.READ, request(path, params, false), "GET", null, null, null), type);
    }

    private <T> T field(JsonNode node, String name, TypeReference<T> type) {
        try {
            return Json.MAPPER.treeToValue(node.path(name), type);
        } catch (IOException e) {
            throw new UnexpectedServerException(
                    "undecodable '" + name + "' member: " + e.getMessage(), null);
        }
    }

    private static <T> T convert(JsonNode node, Class<T> type) {
        try {
            return Json.MAPPER.treeToValue(node, type);
        } catch (IOException e) {
            throw new UnexpectedServerException("undecodable response: " + e.getMessage(), null);
        }
    }

    private static Map<String, String> params() {
        return new LinkedHashMap<>();
    }

    /** Null-guarded QUERY-PARAM put (stringifies); {@link #member} is the
     * JSON-body twin — one guard rule, two value shapes. */
    private static void param(Map<String, String> p, String k, Object v) {
        if (v != null) {
            p.put(k, String.valueOf(v));
        }
    }

    /** Null-guarded JSON-body member put (raw value). */
    private static void member(Map<String, Object> body, String key, Object v) {
        if (v != null) {
            body.put(key, v);
        }
    }

    // -- commands -----------------------------------------------------------

    @Override
    public String createVersion(String parentVersionId, JsonNode metadata, String idem) {
        Map<String, Object> body = new LinkedHashMap<>();
        if (parentVersionId != null) {
            body.put("parent_version_id", parentVersionId);
        }
        if (metadata != null) {
            body.put("metadata", metadata);
        }
        return run(RetryClass.COMMAND, request("/v1/versions", null, false), "POST",
                jsonBody(body), "application/json", idem)
                .path("version_id").asText();
    }

    private static com.fasterxml.jackson.databind.node.ObjectNode claimBody(
            Ledger.ClaimInput c, Long expectedHead) {
        com.fasterxml.jackson.databind.node.ObjectNode body = Json.MAPPER.valueToTree(c);
        if (!body.hasNonNull("claim_type")) {
            // The server REQUIRES claim_type; the gRPC transport defaults an
            // absent one to fact — the REST body must agree or the same
            // ClaimInput would succeed on one transport and 400 on the other.
            body.put("claim_type", "fact");
        }
        if (expectedHead != null) {
            body.put("expected_head", expectedHead);
        }
        return body;
    }

    @Override
    public Ledger.ClaimOutcome proposeClaim(
            String versionId, Ledger.ClaimInput claim, Long expectedHead, String idem) {
        return convert(
                run(RetryClass.COMMAND,
                        request("/v1/versions/" + seg(versionId) + "/claims", null, false),
                        "POST", jsonBody(claimBody(claim, expectedHead)), "application/json", idem),
                Ledger.ClaimOutcome.class);
    }

    @Override
    public Ledger.EventsOutcome appendEvents(String versionId, List<Ledger.ClaimInput> claims,
            String candidateText, Long expectedHead, String idem) {
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("claims", claims.stream().map(c -> claimBody(c, null)).toList());
        if (candidateText != null) {
            body.put("candidate_text", candidateText);
        }
        if (expectedHead != null) {
            body.put("expected_head", expectedHead);
        }
        return convert(
                run(RetryClass.COMMAND,
                        request("/v1/versions/" + seg(versionId) + "/events", null, false),
                        "POST", jsonBody(body), "application/json", idem),
                Ledger.EventsOutcome.class);
    }

    @Override
    public Memory.Promise openPromise(String versionId, Params.PromiseInput p, String idem) {
        return convert(
                run(RetryClass.COMMAND,
                        request("/v1/versions/" + seg(versionId) + "/promises", null, false),
                        "POST", jsonBody(p), "application/json", idem),
                Memory.Promise.class);
    }

    @Override
    public boolean fulfillPromise(String versionId, String key, String idem) {
        return run(RetryClass.COMMAND,
                request("/v1/versions/" + seg(versionId) + "/promises/" + seg(key) + "/fulfill",
                        null, false),
                "POST", jsonBody(Map.of()), "application/json", idem)
                .path("fulfilled").asBoolean();
    }

    @Override
    public Memory.Anchor lockAnchor(String versionId, Params.AnchorInput a, String idem) {
        return convert(
                run(RetryClass.COMMAND,
                        request("/v1/versions/" + seg(versionId) + "/anchors", null, false),
                        "POST", jsonBody(a), "application/json", idem),
                Memory.Anchor.class);
    }

    @Override
    public void recordCounts(String versionId, String key, String scopePath, long count,
            Long budget, String idem) {
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("key", key);
        body.put("scope_path", scopePath);
        body.put("count", count);
        if (budget != null) {
            body.put("budget", budget);
        }
        run(RetryClass.COMMAND,
                request("/v1/versions/" + seg(versionId) + "/counters", null, false),
                "POST", jsonBody(body), "application/json", idem);
    }

    @Override
    public void upsertDigest(Memory.Digest digest) {
        // Upsert by definition — the one command outside idempotency scope.
        run(RetryClass.WRITE_ONCE,
                request("/v1/versions/" + seg(digest.versionId()) + "/digests", null, false),
                "PUT", jsonBody(digest), "application/json", null);
    }

    // -- query --------------------------------------------------------------

    @Override
    public long head(String versionId) {
        return run(RetryClass.READ, request("/v1/versions/" + seg(versionId) + "/head", null, false),
                "GET", null, null, null)
                .path("head_seq").asLong();
    }

    @Override
    public Ledger.ClaimLookup getClaim(String claimId) {
        return get("/v1/claims/" + seg(claimId), null, Ledger.ClaimLookup.class);
    }

    @Override
    public Ledger.FactsPage facts(String versionId, Params.FactsQuery q) {
        var p = params();
        param(p, "scope_prefix", q.scopePrefix());
        param(p, "as_of_seq", q.asOfSeq());
        if (q.statuses() != null && !q.statuses().isEmpty()) {
            p.put("statuses", String.join(",", q.statuses()));
        }
        param(p, "limit", q.limit());
        return get("/v1/versions/" + seg(versionId) + "/facts", p, Ledger.FactsPage.class);
    }

    @Override
    public List<String> lineage(String versionId) {
        return field(
                run(RetryClass.READ,
                        request("/v1/versions/" + seg(versionId) + "/lineage", null, false),
                        "GET", null, null, null),
                "version_ids", new TypeReference<>() {});
    }

    @Override
    public List<Memory.Anchor> anchors(String versionId, Long asOfSeq) {
        var p = params();
        param(p, "as_of_seq", asOfSeq);
        return field(
                run(RetryClass.READ,
                        request("/v1/versions/" + seg(versionId) + "/anchors", p, false),
                        "GET", null, null, null),
                "anchors", new TypeReference<>() {});
    }

    @Override
    public List<Memory.Promise> promises(String versionId, Long asOfSeq, String status) {
        Wire.checkPromiseStatus(status);
        var p = params();
        param(p, "as_of_seq", asOfSeq);
        param(p, "status", status);
        return field(
                run(RetryClass.READ,
                        request("/v1/versions/" + seg(versionId) + "/promises", p, false),
                        "GET", null, null, null),
                "promises", new TypeReference<>() {});
    }

    @Override
    public List<Memory.Counter> counters(String versionId, Long asOfSeq) {
        var p = params();
        param(p, "as_of_seq", asOfSeq);
        return field(
                run(RetryClass.READ,
                        request("/v1/versions/" + seg(versionId) + "/counters", p, false),
                        "GET", null, null, null),
                "counters", new TypeReference<>() {});
    }

    @Override
    public List<Memory.Digest> digests(String versionId) {
        return field(
                run(RetryClass.READ,
                        request("/v1/versions/" + seg(versionId) + "/digests", null, false),
                        "GET", null, null, null),
                "digests", new TypeReference<>() {});
    }

    @Override
    public List<Ledger.StoredFinding> findings(String versionId, Params.FindingsQuery q) {
        var p = params();
        param(p, "as_of_seq", q.asOfSeq());
        param(p, "severity", q.severity());
        param(p, "rule_id", q.ruleId());
        param(p, "limit", q.limit());
        return field(
                run(RetryClass.READ,
                        request("/v1/versions/" + seg(versionId) + "/findings", p, false),
                        "GET", null, null, null),
                "findings", new TypeReference<>() {});
    }

    // -- sealed evidence: reads only --------------------------------

    @Override
    public JsonNode evidence(String evidenceId) {
        return run(RetryClass.READ,
                        request("/v1/evidence/" + seg(evidenceId), params(), false),
                        "GET", null, null, null);
    }

    @Override
    public Evidence.EvidenceRows evidenceRows(String evidenceId, Params.EvidenceRowsQuery q) {
        var p = params();
        param(p, "from", q.from());
        param(p, "limit", q.limit());
        return get("/v1/evidence/" + seg(evidenceId) + "/rows", p, Evidence.EvidenceRows.class);
    }

    @Override
    public Memory.ComposedContext composeContext(String versionId, Params.ContextQuery q) {
        var p = params();
        param(p, "scope", q.scope());
        param(p, "budget_tokens", q.budgetTokens());
        param(p, "fact_limit", q.factLimit());
        param(p, "as_of_seq", q.asOfSeq());
        return get("/v1/versions/" + seg(versionId) + "/context", p, Memory.ComposedContext.class);
    }

    // -- ingest -------------------------------------------------------------

    @Override
    public Ingesting.PutSourceResult putSource(Params.ChunkSource data, Params.SourceMeta meta) {
        // Uploads are idempotent by content address, so transient failures
        // retry — the ChunkSource factory serves a FRESH stream per attempt.
        int attempt = 0;
        while (true) {
            attempt++;
            HttpRequest.Builder b = request("/v1/sources", null, true)
                    .header("content-type",
                            meta.mediaType() != null ? meta.mediaType() : "application/octet-stream");
            if (meta.declaredSha256() != null && !meta.declaredSha256().isEmpty()) {
                b.header("x-content-sha256", meta.declaredSha256());
            }
            if (meta.filename() != null) {
                b.header("x-filename", meta.filename());
            }
            if (meta.shapeRef() != null) {
                b.header("x-shape-ref", meta.shapeRef());
            }
            b.PUT(BodyPublishers.ofInputStream(data::open));
            try {
                return convert(decode(http.send(b.build(), BodyHandlers.ofByteArray())),
                        Ingesting.PutSourceResult.class);
            } catch (IOException e) {
                if (attempt <= readRetries) {
                    Wire.sleepBackoff(attempt);
                    continue;
                }
                throw new MunariumTransportException(e.toString(), delivered(e));
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw new MunariumTransportException("interrupted", true);
            } catch (MunariumException e) {
                if (e.isTransient() && attempt <= readRetries) {
                    Wire.sleepBackoff(attempt);
                    continue;
                }
                throw e;
            }
        }
    }

    @Override
    public Ingesting.RecordIngestResult recordIngest(
            String versionId, String contentHash, String shapeRef) {
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("content_hash", contentHash);
        if (shapeRef != null) {
            body.put("shape_ref", shapeRef);
        }
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/versions/" + seg(versionId) + "/ingests", null, false),
                        "POST", jsonBody(body), "application/json", null),
                Ingesting.RecordIngestResult.class);
    }

    @Override
    public Ingesting.IngestResult ingest(Ingesting.IngestFile file) {
        // File/bulk bodies run to the 256 MiB ceiling — deadline-exempt.
        return convert(
                runLargeJson(request("/v1/ingest", null, true), file),
                Ingesting.IngestResult.class);
    }

    @Override
    public List<Ingesting.IngestResult> ingestBatch(List<Ingesting.IngestFile> files) {
        Wire.checkChunkSize("batch", files.size());
        return field(
                runLargeJson(
                        request("/v1/ingest/batch", null, true), Map.of("files", files)),
                "results", new TypeReference<>() {});
    }

    @Override
    public Ingesting.BulkOpenResult bulkOpen(List<Ingesting.BulkManifestEntry> files, String label) {
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("files", files);
        if (label != null) {
            body.put("label", label);
        }
        return convert(
                runLargeJson(request("/v1/ingest/bulk", null, true), body),
                Ingesting.BulkOpenResult.class);
    }

    @Override
    public Ingesting.BulkChunkResult bulkChunk(String bulkId, List<Ingesting.IngestFile> files) {
        Wire.checkChunkSize("bulk chunk", files.size());
        return convert(
                runLargeJson(
                        request("/v1/ingest/bulk/" + seg(bulkId) + "/chunk", null, true),
                        Map.of("files", files)),
                Ingesting.BulkChunkResult.class);
    }

    @Override
    public Ingesting.BulkStatus bulkStatus(String bulkId, boolean includeNeeded) {
        var p = params();
        if (includeNeeded) {
            p.put("include_needed", "true");
        }
        return get("/v1/ingest/bulk/" + seg(bulkId), p, Ingesting.BulkStatus.class);
    }

    @Override
    public Ingesting.BulkCompleteResult bulkComplete(String bulkId) {
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/ingest/bulk/" + seg(bulkId) + "/complete", null, false),
                        "POST", null, null, null),
                Ingesting.BulkCompleteResult.class);
    }

    @Override
    public Ingesting.SourceInfo getSource(String sourceId) {
        return get("/v1/sources/" + seg(sourceId), null, Ingesting.SourceInfo.class);
    }

    // -- retrieval ----------------------------------------------------------

    @Override
    public Retrieval.SearchResult search(Params.SearchQuery q) {
        Map<String, Object> body = new LinkedHashMap<>();
        member(body, "query", q.query());
        member(body, "shape_ref", q.shapeRef());
        member(body, "top_k", q.topK());
        member(body, "index_version", q.indexVersion());
        member(body, "filter", q.filter());
        // A read that happens to be a POST — same retry class as GETs.
        return convert(
                run(RetryClass.READ, request("/v1/search", null, false),
                        "POST", jsonBody(body), "application/json", null),
                Retrieval.SearchResult.class);
    }

    @Override
    public Retrieval.IndexStatus indexStatus(String shapeRef) {
        return get("/v1/indexes/" + seg(shapeRef), null, Retrieval.IndexStatus.class);
    }

    @Override
    public Retrieval.IndexStatus buildIndex(String shapeRef, String versionId) {
        var p = params();
        param(p, "version_id", versionId);
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/indexes/" + seg(shapeRef) + "/build", p, false),
                        "POST", null, null, null),
                Retrieval.IndexStatus.class);
    }

    @Override
    public Retrieval.CollectionInfo createCollection(Params.CollectionSpec spec) {
        return convert(
                run(RetryClass.WRITE_ONCE, request("/v1/collections", null, false),
                        "POST", jsonBody(spec), "application/json", null),
                Retrieval.CollectionInfo.class);
    }

    @Override
    public List<Retrieval.CollectionInfo> listCollections() {
        return field(
                run(RetryClass.READ, request("/v1/collections", null, false), "GET", null, null, null),
                "collections", new TypeReference<>() {});
    }

    @Override
    public Retrieval.CollectionInfo getCollection(String id) {
        return get("/v1/collections/" + seg(id), null, Retrieval.CollectionInfo.class);
    }

    // -- runbooks + shapes + chronology --------------------------------------

    @Override
    public Runbooks.ApplyShapeResult applyShape(String yaml, String versionId) {
        var p = params();
        param(p, "version_id", versionId);
        return convert(
                run(RetryClass.WRITE_ONCE, request("/v1/shapes", p, false),
                        "POST", yaml.getBytes(StandardCharsets.UTF_8), "text/yaml", null),
                Runbooks.ApplyShapeResult.class);
    }

    @Override
    public String applyRunbook(String yaml) {
        return run(RetryClass.WRITE_ONCE, request("/v1/runbooks", null, false),
                "POST", yaml.getBytes(StandardCharsets.UTF_8), "text/yaml", null)
                .path("runbook_ref").asText();
    }

    @Override
    public Runbooks.RunbookRun runRunbook(String name, String versionId) {
        var p = params();
        param(p, "version_id", versionId);
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/runbooks/" + seg(name) + "/runs", p, false),
                        "POST", null, null, null),
                Runbooks.RunbookRun.class);
    }

    @Override
    public Runbooks.RunStatus getRun(String runId) {
        return get("/v1/runs/" + seg(runId), null, Runbooks.RunStatus.class);
    }

    @Override
    public Runbooks.RunbookRun approveStep(String runId, int ordinal) {
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/runs/" + seg(runId) + "/steps/" + ordinal + "/approve",
                                null, false),
                        "POST", null, null, null),
                Runbooks.RunbookRun.class);
    }

    @Override
    public List<Runbooks.RunbookSummary> list(boolean includeRemoved) {
        var p = params();
        if (includeRemoved) {
            p.put("include_removed", "true");
        }
        return field(
                run(RetryClass.READ, request("/v1/runbooks", p, false), "GET", null, null, null),
                "runbooks", new TypeReference<>() {});
    }

    @Override
    public Runbooks.RunbookInfo getInfo(String name) {
        return get("/v1/runbooks/" + seg(name), null, Runbooks.RunbookInfo.class);
    }

    @Override
    public Runbooks.ValidateResult validate(String yaml, Params.ValidateOptions o) {
        var p = params();
        if (o.suggest()) {
            p.put("suggest", "true");
        }
        param(p, "provider", o.provider());
        param(p, "model", o.model());
        param(p, "tier", o.tier());
        // With suggest=true this spends provider tokens — send once.
        return convert(
                run(RetryClass.WRITE_ONCE, request("/v1/runbooks/validate", p, false),
                        "POST", yaml.getBytes(StandardCharsets.UTF_8), "text/yaml", null),
                Runbooks.ValidateResult.class);
    }

    @Override
    public Runbooks.RemovalRequest removeRequest(String name) {
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/runbooks/" + seg(name) + "/remove-request", null, false),
                        "POST", null, null, null),
                Runbooks.RemovalRequest.class);
    }

    @Override
    public Runbooks.RemovalConfirm removeConfirm(String name, String removalId) {
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/runbooks/" + seg(name) + "/remove-confirm", null, false),
                        "POST", jsonBody(Map.of("removal_id", removalId)), "application/json", null),
                Runbooks.RemovalConfirm.class);
    }

    @Override
    public Runbooks.ChronologyRulesResult applyChronologyRules(String yaml) {
        return convert(
                run(RetryClass.WRITE_ONCE, request("/v1/chronology-rules", null, false),
                        "POST", yaml.getBytes(StandardCharsets.UTF_8), "text/yaml", null),
                Runbooks.ChronologyRulesResult.class);
    }

    @Override
    public String getChronologyRules(String name) {
        // A text (non-JSON) read — same retry class, its own decode.
        int attempt = 0;
        while (true) {
            attempt++;
            try {
                HttpResponse<byte[]> resp = http.send(
                        request("/v1/chronology-rules/" + seg(name), null, false)
                                .GET().build(),
                        BodyHandlers.ofByteArray());
                if (resp.statusCode() >= 200 && resp.statusCode() < 300) {
                    return new String(resp.body(), StandardCharsets.UTF_8);
                }
                throw decodeError(resp);
            } catch (IOException e) {
                if (attempt <= readRetries) {
                    Wire.sleepBackoff(attempt);
                    continue;
                }
                throw new MunariumTransportException(e.toString(), delivered(e));
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                throw new MunariumTransportException("interrupted", true);
            } catch (MunariumException e) {
                if (e.isTransient() && attempt <= readRetries) {
                    Wire.sleepBackoff(attempt);
                    continue;
                }
                throw e;
            }
        }
    }

    // -- providers ----------------------------------------------------------

    @Override
    public String applyConfig(String yaml) {
        return run(RetryClass.WRITE_ONCE, request("/v1/providers", null, false),
                "POST", yaml.getBytes(StandardCharsets.UTF_8), "text/yaml", null)
                .path("config_name").asText();
    }

    @Override
    public Providers.ProviderHealth health(String name) {
        return get("/v1/providers/" + seg(name) + "/health", null, Providers.ProviderHealth.class);
    }

    @Override
    public Providers.HealthAiResult healthAi() {
        return get("/healthai", null, Providers.HealthAiResult.class);
    }

    @Override
    public Providers.CompleteResult complete(String name, Params.CompleteOptions o) {
        Map<String, Object> body = new LinkedHashMap<>();
        member(body, "prompt", o.prompt());
        member(body, "system", o.system());
        member(body, "model", o.model());
        member(body, "provider", o.provider());
        member(body, "tier", o.tier());
        member(body, "max_tokens", o.maxTokens());
        member(body, "temperature", o.temperature());
        member(body, "version_id", o.versionId());
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/providers/" + seg(name) + "/complete", null, false),
                        "POST", jsonBody(body), "application/json", null),
                Providers.CompleteResult.class);
    }

    @Override
    public Providers.EmbedResult embed(String name, Params.EmbedOptions o) {
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("inputs", o.inputs());
        member(body, "model", o.model());
        member(body, "provider", o.provider());
        member(body, "version_id", o.versionId());
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/providers/" + seg(name) + "/embed", null, false),
                        "POST", jsonBody(body), "application/json", null),
                Providers.EmbedResult.class);
    }

    @Override
    public Providers.ProviderList list() {
        return get("/v1/providers", null, Providers.ProviderList.class);
    }

    @Override
    public Providers.MaxTokensResponse maxTokens() {
        return get("/v1/max-tokens", null, Providers.MaxTokensResponse.class);
    }

    @Override
    public Providers.MaxTokensResponse replaceMaxTokens(Providers.MaxTokensBudgets budgets) {
        // The record's eight primitives ARE the wire contract — all
        // required, none nullable — so the body can never be a partial
        // update. Send-once like applyConfig: a replace has no idempotency
        // key, and a possibly-delivered one must not be re-sent blind.
        return convert(
                run(RetryClass.WRITE_ONCE, request("/v1/max-tokens", null, false),
                        "POST", jsonBody(budgets), "application/json", null),
                Providers.MaxTokensResponse.class);
    }

    // -- sessions (incl. the SSE streaming turn) -----------------------------

    @Override
    public SessionsApi.CreateSessionResult create(String runbookName) {
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/runbooks/" + seg(runbookName) + "/sessions", null, false),
                        "POST", null, null, null),
                SessionsApi.CreateSessionResult.class);
    }

    /** Package-visible so the byte-identity test can assert on the body a
     * legacy caller produces without standing up a server. */
    static Map<String, Object> turnBody(Params.TurnOptions o) {
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("query", o.query());
        member(body, "top_k", o.topK());
        member(body, "complete", o.complete());
        member(body, "model_override", o.modelOverride());
        // Null-guarded like every other optional member: a caller who does
        // not use a research profile must send the bytes it always sent.
        member(body, "research_profile", o.researchProfile());
        return body;
    }

    @Override
    public SessionsApi.TurnResult turn(String sessionId, Params.TurnOptions o) {
        // Send-once, never auto-retried, and DEADLINE-EXEMPT: a turn spends
        // provider tokens a client-side abort cannot stop (the transcript
        // ordinal still advances) — a 30 s cap on a capable-tier completion
        // would be a double-spend invitation.
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/sessions/" + seg(sessionId) + "/turns", null, true),
                        "POST", jsonBody(turnBody(o)), "application/json", null),
                SessionsApi.TurnResult.class);
    }

    @Override
    public SessionsApi.TurnResult turnStream(
            String sessionId, Params.TurnOptions o, Consumer<SessionsApi.TurnProgress> onProgress) {
        HttpRequest req = request("/v1/sessions/" + seg(sessionId) + "/turns/stream", null, true)
                // Bound the HEADER wait: java.net.http's request timeout
                // fires if response headers don't arrive in time, and does
                // NOT govern the streamed body after them — so a peer that
                // accepts the connection but never answers cannot hang the
                // caller, while long streams stay unbounded (the idle
                // watchdog owns the body phase).
                .timeout(requestTimeout)
                .header("accept", "text/event-stream")
                .header("content-type", "application/json")
                .POST(BodyPublishers.ofByteArray(jsonBody(turnBody(o))))
                .build();
        HttpResponse<InputStream> resp;
        try {
            resp = http.send(req, BodyHandlers.ofInputStream());
        } catch (IOException e) {
            throw new MunariumTransportException(e.toString(), delivered(e));
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new MunariumTransportException("interrupted", true);
        }
        if (resp.statusCode() < 200 || resp.statusCode() >= 300) {
            // Pre-stream failures (auth, refusals, shed) are plain
            // problem+json — decoded by the ONE error path, Retry-After kept.
            byte[] body;
            try (InputStream in = resp.body()) {
                body = in.readAllBytes();
            } catch (IOException e) {
                throw new UnexpectedServerException(
                        "unreadable error body (HTTP " + resp.statusCode() + ")",
                        resp.statusCode());
            }
            throw decodeError(resp.statusCode(), body, retryAfter(resp));
        }
        return readTurnStream(resp.body(), onProgress);
    }

    /**
     * Consume the SSE body: progress events fire the callback (undecodable
     * PROGRESS data is skipped — a newer server may add stages, and
     * progress is informational — but an undecodable TERMINAL event is an
     * error: the caller was owed a TurnResult); exactly one terminal
     * done/error ends the stream; ending WITHOUT one is a typed transport
     * error, never a silent success. No overall deadline, but a 60 s idle
     * watchdog — the server heartbeats keep-alives every 15 s, so a silent
     * wire means a wedged peer, not a slow completion.
     */
    private SessionsApi.TurnResult readTurnStream(
            InputStream body, Consumer<SessionsApi.TurnProgress> onProgress) {
        return readTurnStream(body, onProgress, SSE_IDLE_TIMEOUT);
    }

    /** Package-visible timeout seam keeps the watchdog's consumer-vs-wire
     * regression test fast; production always calls the two-argument twin. */
    SessionsApi.TurnResult readTurnStream(
            InputStream body,
            Consumer<SessionsApi.TurnProgress> onProgress,
            Duration idleTimeout) {
        // A null callback means "final result only" — never an NPE after
        // the paid turn is already underway.
        Consumer<SessionsApi.TurnProgress> progress = onProgress != null ? onProgress : p -> {};
        SseParser parser = new SseParser();
        byte[] chunk = new byte[8 * 1024];
        AtomicBoolean idleTripped = new AtomicBoolean(false);
        AtomicBoolean readPending = new AtomicBoolean(false);
        AtomicLong readStarted = new AtomicLong(System.nanoTime());
        // NOT try-with-resources: the watchdog closes `in` on purpose to
        // wake a blocked read, and the [try] lint objects to that overlap;
        // the finally below is the single close.
        InputStream in = body;
        // ONE periodic checker per stream (not a schedule/cancel per read —
        // that is timer-queue churn on the hottest loop): every quarter of
        // the idle budget it checks ONLY an in-flight network read. A slow
        // progress callback is application work, not wire silence, and must
        // never make the watchdog close an otherwise healthy stream.
        ScheduledFuture<?> checker = watchdog.scheduleAtFixedRate(() -> {
            if (readPending.get()
                    && System.nanoTime() - readStarted.get() > idleTimeout.toNanos()) {
                idleTripped.set(true);
                try {
                    in.close(); // wakes the blocked read
                } catch (IOException ignored) {
                    // closing an already-broken stream is fine
                }
            }
        }, Math.max(idleTimeout.toMillis() / 4, 1), Math.max(idleTimeout.toMillis() / 4, 1),
                TimeUnit.MILLISECONDS);
        try {
            while (true) {
                readStarted.set(System.nanoTime());
                readPending.set(true);
                int n;
                try {
                    n = in.read(chunk);
                } finally {
                    readPending.set(false);
                }
                if (n < 0) {
                    throw new MunariumTransportException(
                            "SSE stream ended without a terminal done/error event", true);
                }
                for (SseParser.Event ev : parser.push(chunk, n)) {
                    switch (ev.event()) {
                        case "progress" -> {
                            try {
                                progress.accept(Json.MAPPER.readValue(
                                        ev.data(), SessionsApi.TurnProgress.class));
                            } catch (IOException skipped) {
                                // forward-compat: informational, skip
                            }
                        }
                        case "done" -> {
                            try {
                                return Json.MAPPER.readValue(ev.data(), SessionsApi.TurnResult.class);
                            } catch (IOException e) {
                                throw new UnexpectedServerException(
                                        "undecodable SSE done event: " + e.getMessage(), null);
                            }
                        }
                        case "error" -> {
                            // The same problem+json the unary route would
                            // have returned — decode through the one registry.
                            JsonNode problem;
                            try {
                                problem = Json.MAPPER.readTree(ev.data());
                            } catch (IOException e) {
                                throw new UnexpectedServerException(
                                        "undecodable SSE error event: " + e.getMessage(), null);
                            }
                            throw Problems.fromProblemJson(
                                    problem.path("status").asInt(500), problem, null);
                        }
                        default -> {
                            // unnamed/unknown events: ignored (forward-compat)
                        }
                    }
                }
            }
        } catch (IOException e) {
            if (idleTripped.get()) {
                throw new MunariumTransportException(
                        "SSE stream idle for " + idleTimeout.toSeconds()
                                + "s (the server heartbeats every 15s) — wedged peer",
                        true);
            }
            throw new MunariumTransportException(e.toString(), true);
        } catch (SseParser.Overflow e) {
            throw new UnexpectedServerException(e.getMessage(), null);
        } finally {
            checker.cancel(false);
            try {
                in.close();
            } catch (IOException ignored) {
                // already closed by the watchdog, or broken — either is fine
            }
        }
    }

    @Override
    public SessionsApi.Session get(String sessionId) {
        return get("/v1/sessions/" + seg(sessionId), null, SessionsApi.Session.class);
    }

    @Override
    public SessionsApi.Session close(String sessionId) {
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/sessions/" + seg(sessionId) + "/close", null, false),
                        "POST", null, null, null),
                SessionsApi.Session.class);
    }

    // -- access tokens (mgmt) ------------------------------------------------

    @Override
    public Tokens.TokenGrant mint(Tokens.IssueTokenRequest r) {
        // Minting twice issues two live tokens — send once.
        Map<String, Object> body = new LinkedHashMap<>();
        body.put("uid", r.uid());
        body.put("access_level", r.accessLevel());
        body.put("compartments", r.compartments() == null ? List.of() : r.compartments());
        body.put("scopes", r.scopes());
        member(body, "runbook_refs", r.runbookRefs());
        member(body, "ttl_secs", r.ttlSecs());
        return convert(
                run(RetryClass.WRITE_ONCE, request("/v1/access-tokens", null, false),
                        "POST", jsonBody(body), "application/json", null),
                Tokens.TokenGrant.class);
    }

    @Override
    public List<Tokens.TokenInfo> list(Params.TokenListQuery q) {
        var p = params();
        param(p, "uid", q.uid());
        param(p, "active", q.active());
        return field(
                run(RetryClass.READ, request("/v1/access-tokens", p, false), "GET", null, null, null),
                "tokens", new TypeReference<>() {});
    }

    @Override
    public Tokens.RevokeResult revoke(String jti) {
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/access-tokens/" + seg(jti) + "/revoke", null, false),
                        "POST", null, null, null),
                Tokens.RevokeResult.class);
    }

    // -- reports (mgmt) ------------------------------------------------------

    @Override
    public Reports.UsageReport usage(Params.UsageQuery q) {
        var p = params();
        param(p, "group_by", q.groupBy());
        param(p, "from", q.from());
        param(p, "to", q.to());
        return get("/v1/reports/usage", p, Reports.UsageReport.class);
    }

    @Override
    public Reports.AuditPage audit(Params.AuditQuery q) {
        var p = params();
        param(p, "uid", q.uid());
        param(p, "session_id", q.sessionId());
        param(p, "runbook", q.runbook());
        param(p, "from", q.from());
        param(p, "to", q.to());
        param(p, "limit", q.limit());
        if (q.bodies()) {
            p.put("bodies", "true");
        }
        param(p, "before", q.before());
        return get("/v1/reports/audit", p, Reports.AuditPage.class);
    }

    @Override
    public Reports.CostReport cost(String from, String to) {
        var p = params();
        param(p, "from", from);
        param(p, "to", to);
        return get("/v1/reports/cost", p, Reports.CostReport.class);
    }

    @Override
    public Reports.TimeseriesReport timeseries(String window, String plane) {
        var p = params();
        param(p, "window", window);
        param(p, "plane", plane);
        return get("/v1/reports/timeseries", p, Reports.TimeseriesReport.class);
    }

    @Override
    public Reports.EndpointsReport endpoints(String window, Long limit) {
        var p = params();
        param(p, "window", window);
        param(p, "limit", limit);
        return get("/v1/reports/endpoints", p, Reports.EndpointsReport.class);
    }

    @Override
    public Reports.RunbookReport runbooks(String window) {
        var p = params();
        param(p, "window", window);
        return get("/v1/reports/runbooks", p, Reports.RunbookReport.class);
    }

    @Override
    public Reports.SessionsReport sessions(String window) {
        var p = params();
        param(p, "window", window);
        return get("/v1/reports/sessions", p, Reports.SessionsReport.class);
    }

    @Override
    public Reports.EvidenceReport evidenceReport(String window) {
        var p = params();
        param(p, "window", window);
        return get("/v1/reports/evidence", p, Reports.EvidenceReport.class);
    }

    @Override
    public Reports.MatrixReport matrix() {
        // No window: the breaker reading is instantaneous instance state,
        // not an aggregate over a period.
        return get("/v1/reports/matrix", null, Reports.MatrixReport.class);
    }

    // -- authoring -----------------------------------------------------------

    @Override
    public Authoring.PatternPage listPatterns() {
        return get("/v1/authoring/patterns", null, Authoring.PatternPage.class);
    }

    @Override
    public Authoring.PatternDetail getPattern(String id) {
        return get("/v1/authoring/patterns/" + seg(id), null, Authoring.PatternDetail.class);
    }

    @Override
    public Authoring.Draft createDraft(Authoring.CreateDraftRequest r) {
        return convert(
                run(RetryClass.WRITE_ONCE, request("/v1/authoring/drafts", null, false),
                        "POST", jsonBody(r), "application/json", null),
                Authoring.Draft.class);
    }

    @Override
    public Authoring.DraftPage listDrafts() {
        return get("/v1/authoring/drafts", null, Authoring.DraftPage.class);
    }

    @Override
    public Authoring.Draft getDraft(String draftId) {
        return get("/v1/authoring/drafts/" + seg(draftId), null, Authoring.Draft.class);
    }

    @Override
    public Authoring.DraftDelete deleteDraft(String draftId) {
        // The client surface's one DELETE — soft workspace cleanup.
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/authoring/drafts/" + seg(draftId), null, false),
                        "DELETE", null, null, null),
                Authoring.DraftDelete.class);
    }

    @Override
    public Authoring.Draft putAnswers(String draftId, JsonNode answers, boolean materialize) {
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/authoring/drafts/" + seg(draftId) + "/answers", null, false),
                        "PUT", jsonBody(Map.of("answers", answers, "materialize", materialize)),
                        "application/json", null),
                Authoring.Draft.class);
    }

    @Override
    public Authoring.DraftValidation validate(String draftId) {
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/authoring/drafts/" + seg(draftId) + "/validate", null, false),
                        "POST", jsonBody(Map.of()), "application/json", null),
                Authoring.DraftValidation.class);
    }

    @Override
    public Authoring.AssistResult assist(String draftId, Authoring.AssistRequest r) {
        // A BYOK provider call rides behind this — send once.
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/authoring/drafts/" + seg(draftId) + "/assist", null, false),
                        "POST", jsonBody(r), "application/json", null),
                Authoring.AssistResult.class);
    }

    @Override
    public Authoring.ExportBundle export(String draftId) {
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/authoring/drafts/" + seg(draftId) + "/export", null, false),
                        "POST", jsonBody(Map.of()), "application/json", null),
                Authoring.ExportBundle.class);
    }

    @Override
    public Authoring.ApplyDraftResult apply(String draftId) {
        return convert(
                run(RetryClass.WRITE_ONCE,
                        request("/v1/authoring/drafts/" + seg(draftId) + "/apply", null, false),
                        "POST", jsonBody(Map.of()), "application/json", null),
                Authoring.ApplyDraftResult.class);
    }

    // -- meta ---------------------------------------------------------------

    public Meta.ServerVersion serverVersion() {
        return get("/version", null, Meta.ServerVersion.class);
    }
}
