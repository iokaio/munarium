// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.JsonNode;
import java.io.IOException;
import java.net.URI;
import java.net.URLEncoder;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpRequest.BodyPublisher;
import java.net.http.HttpRequest.BodyPublishers;
import java.net.http.HttpResponse;
import java.net.http.HttpResponse.BodyHandlers;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * The official Java client for <b>Munarium Matrix</b>, the structured-evidence
 * plane. Synchronous; {@link AsyncMatrixClient} is the {@code CompletableFuture}
 * twin over the same code.
 *
 * <p>It speaks Matrix's REST API and nothing else. <b>There is no gRPC
 * transport here.</b> Matrix's gRPC plane serves {@code MatrixQuery/Execute}
 * alone, and that call is service-to-service: the munarium-server makes it
 * while answering a turn, carrying a session's authorization snapshot an
 * application does not hold. When Matrix grows an RPC an application is
 * entitled to make, this class grows a transport rather than acquiring a
 * sibling.
 *
 * <p>Three absences are design decisions, not gaps:
 *
 * <ul>
 *   <li><b>No sealing.</b> No method on this class seals evidence. A manifest
 *       is a statement about work the <i>sealer</i> did; an SDK offering
 *       {@code sealEvidence} would invite an application to assert provenance
 *       it cannot vouch for. Sealing is Matrix's own act. Evidence is
 *       <i>read</i> through the <b>server's</b> client, resolving an
 *       {@code [evidence/<id>#<row>]} citation.
 *   <li><b>No local validation.</b> {@link #validate(String)} posts the YAML
 *       and returns Matrix's own findings. A client carrying its own copy of
 *       the rules would drift from the service that enforces them, and the
 *       drift would surface as an asset that validates here and is refused
 *       there.
 *   <li><b>No SQL.</b> Nothing on this surface takes a statement. Queries are
 *       pre-declared contracts and views, executed by name.
 * </ul>
 *
 * <p>Nothing here retries, and that is a real trade-off rather than an
 * oversight. Matrix's mutating routes carry no idempotency-key contract, so a
 * blind re-send of an accepted {@code sync} queues a second one; meanwhile the
 * refusal class already tells a caller when trying again is meaningful
 * ({@link MatrixException#retryable()}). A built-in policy that ignored that
 * distinction would be worse than none, so the decision lands on never doing
 * twice what the caller asked once.
 *
 * <p>Instances are safe to share across threads; the underlying
 * {@code HttpClient} is.
 */
public final class MatrixClient implements AutoCloseable {

    private final HttpClient http;
    private final String base;
    private final Duration requestTimeout;
    private final String token;
    private final String uid;

    public MatrixClient(MatrixClientOptions options) {
        this.http = HttpClient.newBuilder().connectTimeout(options.connectTimeout()).build();
        this.base = options.endpoint().replaceAll("/+$", "");
        this.requestTimeout = options.requestTimeout();
        this.token = options.token();
        this.uid = options.uid();
    }

    public static MatrixClient of(String endpoint) {
        return new MatrixClient(MatrixClientOptions.of(endpoint));
    }

    public static MatrixClient of(String endpoint, String token) {
        return new MatrixClient(MatrixClientOptions.of(endpoint).withToken(token));
    }

    @Override
    public void close() {
        // Java 21's HttpClient is AutoCloseable: without this, each client
        // leaks a selector-manager thread and its pooled sockets until GC.
        http.close();
    }

    // -- meta -----------------------------------------------------------------

    /**
     * {@code GET /version} — including whether this Matrix's server agrees with
     * it about the contract. See {@link Version#lockstepOk()}.
     */
    public Version version() {
        return read("GET", "/version", null, Version.class);
    }

    /**
     * Liveness. Answers {@code false} rather than throwing: "is it up?" turned
     * into an exception is a question the caller has to wrap in a try just to
     * get a boolean back out of.
     */
    public boolean healthz() {
        try {
            exchange("GET", "/healthz", null, null, null);
            return true;
        } catch (MatrixException e) {
            return false;
        }
    }

    /**
     * Which sources are REGISTERED — see {@link HealthData}, which deliberately
     * is not a connectivity check.
     */
    public HealthData healthdata() {
        return read("GET", "/healthdata", null, HealthData.class);
    }

    // -- registry -------------------------------------------------------------

    /**
     * Apply one asset, kind-sniffed by Matrix from its own {@code kind:} line.
     * See {@link ApplyOutcome} for what {@code unchanged} means.
     */
    public ApplyOutcome apply(String yaml) {
        return writeYaml("POST", "/v1/assets", yaml, ApplyOutcome.class);
    }

    /** Matrix's own validators, run without applying anything. */
    public Validation validate(String yaml) {
        return writeYaml("POST", "/v1/assets/validate", yaml, Validation.class);
    }

    /**
     * List a registry. {@code kind} is the route segment: {@code datasources},
     * {@code contracts}, {@code mappings}, {@code metricviews},
     * {@code dataviews}.
     */
    public List<AssetSummary> listAssets(String kind) {
        return listAssets(kind, false);
    }

    /** {@code allVersions} asks for the history rather than the current version of each name. */
    public List<AssetSummary> listAssets(String kind, boolean allVersions) {
        // The parameter Matrix reads is `all_versions`. Spelling it `all` sends
        // something the service silently ignores, and a listing that is quietly
        // latest-only is indistinguishable from a registry with no history.
        String query = allVersions ? params(Map.of("all_versions", "true")) : null;
        JsonNode root = readTree("GET", "/v1/" + seg(kind), query);
        return convert(root.path("assets"), new TypeReference<List<AssetSummary>>() {});
    }

    /**
     * The applied YAML back, verbatim — the bytes Matrix stored, not a
     * re-serialisation of a parse. A round trip through the parsed form would
     * silently normalise an operator's file, and those stored bytes are what an
     * asset's identity is computed over.
     */
    public String getYaml(String kind, String name) {
        HttpResponse<byte[]> resp =
                exchange("GET", "/v1/" + seg(kind) + "/" + seg(name), null, null, null);
        return new String(resp.body(), StandardCharsets.UTF_8);
    }

    // -- sources --------------------------------------------------------------

    /**
     * What the source exposes, and what the configured role can actually do
     * there.
     *
     * <p>Left as a {@link JsonNode} deliberately: the role-posture report and
     * the table list are still moving, and mirroring an unsettled shape would
     * put a second normative copy of it in this client.
     */
    public JsonNode introspect(String source) {
        return readTree("POST", "/v1/datasources/" + seg(source) + "/introspect", null);
    }

    /**
     * Reachability now. An unreachable source is an ANSWER here, not an
     * exception — see {@link Probe}.
     */
    public Probe probe(String source) {
        return read("POST", "/v1/datasources/" + seg(source) + "/probe", null, Probe.class);
    }

    /** Enqueue a materialization pass (mode A). */
    public JobAccepted sync(String source) {
        return read("POST", "/v1/datasources/" + seg(source) + "/sync", null, JobAccepted.class);
    }

    // -- contracts and views --------------------------------------------------

    /**
     * Run a query contract's verified questions — its regression suite. The
     * call succeeding and the CONTRACT passing are different things: read
     * {@link VerifyOutcome#failed()}.
     */
    public VerifyOutcome verify(String contract) {
        return read("POST", "/v1/contracts/" + seg(contract) + "/verify", null, VerifyOutcome.class);
    }

    /**
     * The same for a metric view or a native data view, recording the
     * definition fingerprint the questions ran under.
     *
     * <p>A metric view first, a data view when there is none by that name: the
     * caller names the VIEW, not the route it happens to live on. Which of the
     * two kinds a view is, is an authoring detail, and making every call site
     * track it would mean every call site knows something it has no reason to.
     */
    public VerifyOutcome verifyView(String view) {
        try {
            return read("POST", "/v1/metricviews/" + seg(view) + "/verify", null, VerifyOutcome.class);
        } catch (MatrixException e) {
            if (e.status() == null || e.status() != 404) {
                throw e;
            }
            return read("POST", "/v1/dataviews/" + seg(view) + "/verify", null, VerifyOutcome.class);
        }
    }

    // -- reconcile ------------------------------------------------------------

    /** Enqueue a reconcile pass (mode C). */
    public JobAccepted reconcile(String mapping) {
        return read("POST", "/v1/mappings/" + seg(mapping) + "/run", null, JobAccepted.class);
    }

    /** Where a mapping stands against the promotion gates, promoted or not. */
    public PromotionStatus promotionStatus(String mapping) {
        return read("GET", "/v1/mappings/" + seg(mapping) + "/promotion", null, PromotionStatus.class);
    }

    /** The gate measurements over time, at whatever limit the service defaults to. */
    public GateHistory gateHistory(String mapping) {
        return gateHistory(mapping, 0);
    }

    /** A {@code limit} of zero or less leaves the count to the service. */
    public GateHistory gateHistory(String mapping, int limit) {
        String query = limit > 0 ? params(Map.of("limit", String.valueOf(limit))) : null;
        return read("GET", "/v1/mappings/" + seg(mapping) + "/gate-history", query, GateHistory.class);
    }

    /**
     * Let a mapping's claims reach the ledger.
     *
     * <p>The gates — identity precision, value conformance — are checked by
     * MATRIX at the decision, not here. A client that pre-checked them would be
     * a second opinion nobody audited, and it would disagree with the service
     * the moment a threshold moved.
     *
     * <p>{@code decisionId} is the operator's own record — a ticket, a change
     * number — and it is required, because a promotion nobody can trace to a
     * decision is a promotion nobody made.
     */
    public PromotionStatus promote(String mapping, String decisionId, String actor, String reason) {
        Map<String, String> body = new LinkedHashMap<>();
        body.put("decision_id", decisionId);
        if (actor != null) {
            body.put("actor", actor);
        }
        if (reason != null) {
            body.put("reason", reason);
        }
        return writeJson("POST", "/v1/mappings/" + seg(mapping) + "/promote", body, PromotionStatus.class);
    }

    /** As above, with no stated reason. */
    public PromotionStatus promote(String mapping, String decisionId, String actor) {
        return promote(mapping, decisionId, actor, null);
    }

    /**
     * Stop the writes. Nothing already proposed is touched — that is what
     * {@link #rollback} is for.
     */
    public PromotionStatus demote(String mapping, String decisionId) {
        return writeJson("POST", "/v1/mappings/" + seg(mapping) + "/demote",
                Map.of("decision_id", decisionId), PromotionStatus.class);
    }

    /**
     * Undo what a promoted mapping wrote — by SUPERSESSION, never by deletion.
     * History is not rewritten.
     */
    public RollbackOutcome rollback(String mapping, String decisionId) {
        return writeJson("POST", "/v1/mappings/" + seg(mapping) + "/rollback",
                Map.of("decision_id", decisionId), RollbackOutcome.class);
    }

    // -- audit ----------------------------------------------------------------

    /** The 50 most recent journal entries. */
    public List<JsonNode> journal() {
        return journal(50);
    }

    /**
     * Every operation, redacted by default: parameters and results never
     * appear, only what happened and how it ended.
     *
     * <p>Entries stay {@link JsonNode} because their shape varies by operation
     * kind. Flattening them into one record would either lose fields or invent
     * a union that nothing on the wire actually writes.
     */
    public List<JsonNode> journal(int limit) {
        JsonNode root = readTree("GET", "/v1/journal", params(Map.of("limit", String.valueOf(limit))));
        JsonNode entries = root.isArray() ? root : root.path("entries");
        List<JsonNode> out = new ArrayList<>();
        entries.forEach(out::add);
        return List.copyOf(out);
    }

    // -- plumbing -------------------------------------------------------------

    private static String seg(String s) {
        // Asset names are free-form enough that a stray '/' or '?' must not be
        // able to change the route shape. (URLEncoder is form-encoding: undo
        // the '+' it produces for a space.)
        return URLEncoder.encode(s, StandardCharsets.UTF_8).replace("+", "%20");
    }

    private static String params(Map<String, String> values) {
        StringBuilder sb = new StringBuilder();
        for (Map.Entry<String, String> e : values.entrySet()) {
            if (!sb.isEmpty()) {
                sb.append('&');
            }
            sb.append(URLEncoder.encode(e.getKey(), StandardCharsets.UTF_8))
                    .append('=')
                    .append(URLEncoder.encode(e.getValue(), StandardCharsets.UTF_8));
        }
        return sb.toString();
    }

    private <T> T read(String method, String path, String query, Class<T> type) {
        return decode(exchange(method, path, query, null, null), type);
    }

    private JsonNode readTree(String method, String path, String query) {
        return decode(exchange(method, path, query, null, null), JsonNode.class);
    }

    private <T> T writeYaml(String method, String path, String yaml, Class<T> type) {
        BodyPublisher body = BodyPublishers.ofString(yaml, StandardCharsets.UTF_8);
        return decode(exchange(method, path, null, body, "text/yaml"), type);
    }

    private <T> T writeJson(String method, String path, Object body, Class<T> type) {
        byte[] bytes;
        try {
            bytes = MatrixJson.MAPPER.writeValueAsBytes(body);
        } catch (IOException e) {
            throw new MatrixException(
                    "unserializable request body: " + e.getMessage(), null, "invalid", null, null);
        }
        return decode(
                exchange(method, path, null, BodyPublishers.ofByteArray(bytes), "application/json"),
                type);
    }

    private HttpResponse<byte[]> exchange(
            String method, String path, String query, BodyPublisher body, String contentType) {
        URI uri = URI.create(base + path + (query == null || query.isEmpty() ? "" : "?" + query));
        HttpRequest.Builder b = HttpRequest.newBuilder(uri).timeout(requestTimeout);
        if (token != null) {
            b.header("authorization", "Bearer " + token);
        }
        if (uid != null) {
            b.header("x-munarium-uid", uid);
        }
        if (contentType != null) {
            b.header("content-type", contentType);
        }
        b.method(method, body == null ? BodyPublishers.noBody() : body);
        HttpResponse<byte[]> resp;
        try {
            resp = http.send(b.build(), BodyHandlers.ofByteArray());
        } catch (IOException e) {
            throw MatrixException.transportFailure(e);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw MatrixException.transportFailure(e);
        }
        if (resp.statusCode() >= 200 && resp.statusCode() < 300) {
            return resp;
        }
        throw MatrixException.from(resp.statusCode(), resp.body());
    }

    private static <T> T decode(HttpResponse<byte[]> resp, Class<T> type) {
        try {
            return MatrixJson.MAPPER.readValue(resp.body(), type);
        } catch (IOException e) {
            throw new MatrixException(
                    "undecodable success body: " + e.getMessage(), resp.statusCode(), null, null, null);
        }
    }

    private static <T> T convert(JsonNode node, TypeReference<T> type) {
        JsonNode source = node;
        if (source == null || source.isMissingNode() || source.isNull()) {
            source = MatrixJson.MAPPER.createArrayNode();
        }
        return MatrixJson.MAPPER.convertValue(source, type);
    }
}
