// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.rest;

import static java.nio.charset.StandardCharsets.UTF_8;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNotEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import com.sun.net.httpserver.HttpServer;
import io.ioka.munarium.client.AsyncMunariumClient;
import io.ioka.munarium.client.MunariumClientOptions;
import io.ioka.munarium.client.errors.ForbiddenException;
import io.ioka.munarium.client.errors.InvalidInputException;
import io.ioka.munarium.client.errors.UnsupportedTransportException;
import io.ioka.munarium.client.grpc.GrpcTransport;
import io.ioka.munarium.client.model.Json;
import io.ioka.munarium.client.model.Providers;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.CompletionException;
import org.junit.jupiter.api.Test;

/**
 * The {@code /v1/max-tokens} pair on the providers plane. The wire
 * assertions run against a loopback {@link HttpServer} that records what it
 * was sent, so the claims are about the bytes that leave the process — in
 * particular that a replace can never be a partial update.
 */
class MaxTokensTest {

    private static final List<String> WIRE_MEMBERS = List.of(
            "turn_completion", "query_expansion", "complete_default", "healthai_probe",
            "hierarchy_classifier", "hierarchy_intent", "runbook_advisory", "authoring_assist");

    private static final String TENANT_BODY = "{\"turn_completion\":4096,"
            + "\"query_expansion\":128,\"complete_default\":1024,\"healthai_probe\":512,"
            + "\"hierarchy_classifier\":32,\"hierarchy_intent\":480,"
            + "\"runbook_advisory\":2048,\"authoring_assist\":8192,"
            + "\"source\":\"tenant\",\"updated_at\":\"2026-09-02T10:15:30Z\"}";

    private static final String ENVIRONMENT_BODY = "{\"turn_completion\":2048,"
            + "\"query_expansion\":256,\"complete_default\":1024,\"healthai_probe\":512,"
            + "\"hierarchy_classifier\":32,\"hierarchy_intent\":480,"
            + "\"runbook_advisory\":2048,\"authoring_assist\":8192,"
            + "\"source\":\"environment\"}";

    private static final Providers.MaxTokensBudgets TENANT_SET =
            new Providers.MaxTokensBudgets(4096, 128, 1024, 512, 32, 480, 2048, 8192);

    // -- GET ----------------------------------------------------------------

    @Test
    void getDecodesEveryMemberAndTheProvenance() throws Exception {
        try (var server = new Recorder(200, "application/json", TENANT_BODY);
                var transport = transport(server)) {
            var got = transport.maxTokens();
            assertEquals("GET", server.method());
            assertEquals("/v1/max-tokens", server.path());
            assertNull(server.query());
            assertEquals(0, server.body().length, "a read sends no body");
            assertEquals(4096L, got.turnCompletion());
            assertEquals(128L, got.queryExpansion());
            assertEquals(1024L, got.completeDefault());
            assertEquals(512L, got.healthaiProbe());
            assertEquals(32L, got.hierarchyClassifier());
            assertEquals(480L, got.hierarchyIntent());
            assertEquals(2048L, got.runbookAdvisory());
            assertEquals(8192L, got.authoringAssist());
            assertEquals("tenant", got.source());
            assertEquals("2026-09-02T10:15:30Z", got.updatedAt());
            // budgets() is a seam, not a member: re-serializing the answer
            // must not grow a key the server never sent.
            assertFalse(Json.MAPPER.writeValueAsString(got).contains("budgets"));
        }
    }

    @Test
    void environmentSourceCarriesNoUpdatedAt() throws Exception {
        try (var server = new Recorder(200, "application/json", ENVIRONMENT_BODY);
                var transport = transport(server)) {
            var got = transport.maxTokens();
            assertEquals("environment", got.source());
            assertNull(got.updatedAt(), "an absent updated_at must not read as an empty instant");
            assertEquals(2048L, got.turnCompletion());
        }
    }

    // -- POST ---------------------------------------------------------------

    @Test
    void replaceSendsAllEightMembersAndDecodesTheAnswer() throws Exception {
        try (var server = new Recorder(200, "application/json", TENANT_BODY);
                var transport = transport(server)) {
            var got = transport.replaceMaxTokens(TENANT_SET);

            assertEquals("POST", server.method());
            assertEquals("/v1/max-tokens", server.path());
            assertEquals("application/json", server.contentType());

            JsonNode sent = Json.MAPPER.readTree(server.body());
            var keys = new ArrayList<String>();
            sent.fieldNames().forEachRemaining(keys::add);
            assertEquals(WIRE_MEMBERS, keys,
                    "the route replaces the WHOLE set: all eight, in order, nothing else");
            assertEquals(4096, sent.get("turn_completion").asInt());
            assertEquals(128, sent.get("query_expansion").asInt());
            assertEquals(1024, sent.get("complete_default").asInt());
            assertEquals(512, sent.get("healthai_probe").asInt());
            assertEquals(32, sent.get("hierarchy_classifier").asInt());
            assertEquals(480, sent.get("hierarchy_intent").asInt());
            assertEquals(2048, sent.get("runbook_advisory").asInt());
            assertEquals(8192, sent.get("authoring_assist").asInt());

            assertEquals("tenant", got.source());
            assertEquals("2026-09-02T10:15:30Z", got.updatedAt());
            assertEquals(TENANT_SET, got.budgets());
        }
    }

    @Test
    void aGetBodyRoundTripsIntoAReplaceBody() throws Exception {
        // The read-modify-write seam: no partial update exists, so a caller
        // reads, changes one member, and sends the whole set back.
        Providers.MaxTokensBudgets edited;
        try (var server = new Recorder(200, "application/json", ENVIRONMENT_BODY);
                var transport = transport(server)) {
            edited = transport.maxTokens().budgets().withTurnCompletion(4096);
        }
        assertEquals(4096L, edited.turnCompletion());
        assertEquals(256L, edited.queryExpansion(), "only the named member changes");
        assertNotEquals(TENANT_SET, edited);

        try (var server = new Recorder(200, "application/json", TENANT_BODY);
                var transport = transport(server)) {
            transport.replaceMaxTokens(edited);
            JsonNode sent = Json.MAPPER.readTree(server.body());
            assertEquals(8, sent.size());
            assertEquals(4096, sent.get("turn_completion").asInt());
            assertEquals(256, sent.get("query_expansion").asInt());
            assertNull(sent.get("source"), "provenance is the server's to state, never sent");
            assertNull(sent.get("updated_at"));
        }
    }

    // -- errors: problem+json through the slug registry ---------------------

    @Test
    void anOutOfRangeReplaceDecodesToInvalidInput() throws Exception {
        String problem = "{\"type\":\"https://munarium.ioka.io/problems/invalid-input\","
                + "\"title\":\"invalid-input\",\"status\":400,"
                + "\"detail\":\"turn_completion must be 256..=16384 (got 7)\"}";
        try (var server = new Recorder(400, "application/problem+json", problem);
                var transport = transport(server)) {
            var e = assertThrows(InvalidInputException.class,
                    () -> transport.replaceMaxTokens(TENANT_SET.withTurnCompletion(7)));
            assertEquals("invalid-input", e.slug());
            assertTrue(e.getMessage().contains("256..=16384"), e.getMessage());
        }
    }

    @Test
    void aNonRwReplaceDecodesToForbidden() throws Exception {
        String problem = "{\"type\":\"https://munarium.ioka.io/problems/forbidden\","
                + "\"title\":\"forbidden\",\"status\":403,"
                + "\"detail\":\"static rw role required\"}";
        try (var server = new Recorder(403, "application/problem+json", problem);
                var transport = transport(server)) {
            assertThrows(ForbiddenException.class, () -> transport.replaceMaxTokens(TENANT_SET));
        }
    }

    // -- gRPC: REST-only, refused honestly -----------------------------------

    @Test
    void grpcTransportRefusesBothAsUnsupported() {
        // Building a channel never connects, so :1 is never dialled.
        try (var grpc = new GrpcTransport(
                MunariumClientOptions.of("127.0.0.1:1").withToken("t").withUid("u"))) {
            var read = assertThrows(UnsupportedTransportException.class, grpc::maxTokens);
            assertTrue(read.getMessage().contains("GET /v1/max-tokens"), read.getMessage());
            var write = assertThrows(UnsupportedTransportException.class,
                    () -> grpc.replaceMaxTokens(TENANT_SET));
            assertTrue(write.getMessage().contains("POST /v1/max-tokens"), write.getMessage());
        }
    }

    // -- async facade: same types, offloaded ---------------------------------

    @Test
    void asyncFacadeCarriesBothWithTheSameTypedFailures() throws Exception {
        try (var server = new Recorder(200, "application/json", TENANT_BODY);
                var client = AsyncMunariumClient.rest(
                        MunariumClientOptions.of(server.url()).withToken("t").withUid("u"))) {
            assertEquals("tenant", client.providers.maxTokens().join().source());
            assertEquals(TENANT_SET, client.providers.replaceMaxTokens(TENANT_SET).join().budgets());
            assertEquals("POST", server.method());
        }

        String problem = "{\"type\":\"https://munarium.ioka.io/problems/invalid-input\","
                + "\"title\":\"invalid-input\",\"status\":400,\"detail\":\"missing field\"}";
        try (var server = new Recorder(400, "application/problem+json", problem);
                var client = AsyncMunariumClient.rest(
                        MunariumClientOptions.of(server.url()).withToken("t").withUid("u"))) {
            var e = assertThrows(CompletionException.class,
                    () -> client.providers.replaceMaxTokens(TENANT_SET).join());
            assertInstanceOf(InvalidInputException.class, e.getCause());
        }
    }

    // -- fixtures ------------------------------------------------------------

    private static RestTransport transport(Recorder server) {
        return new RestTransport(
                MunariumClientOptions.of(server.url()).withToken("t").withUid("u"));
    }

    /**
     * A loopback server that records the request and answers one canned
     * status + body. Deliberately not a stub of RestTransport's internals:
     * the claims under test are about the bytes that leave the process.
     */
    private static final class Recorder implements AutoCloseable {
        private final HttpServer server;
        private final int status;
        private final String contentTypeOut;
        private final byte[] response;
        private volatile String method;
        private volatile String path;
        private volatile String query;
        private volatile String contentType;
        private volatile byte[] body = new byte[0];

        Recorder(int status, String contentTypeOut, String response) throws IOException {
            this.status = status;
            this.contentTypeOut = contentTypeOut;
            this.response = response.getBytes(UTF_8);
            this.server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
            server.createContext("/", exchange -> {
                method = exchange.getRequestMethod();
                path = exchange.getRequestURI().getPath();
                query = exchange.getRequestURI().getQuery();
                contentType = exchange.getRequestHeaders().getFirst("content-type");
                try (InputStream in = exchange.getRequestBody()) {
                    body = in.readAllBytes();
                }
                exchange.getResponseHeaders().add("content-type", this.contentTypeOut);
                exchange.sendResponseHeaders(this.status, this.response.length);
                try (OutputStream out = exchange.getResponseBody()) {
                    out.write(this.response);
                }
            });
            server.start();
        }

        String url() {
            return "http://127.0.0.1:" + server.getAddress().getPort();
        }

        String method() {
            return method;
        }

        String path() {
            return path;
        }

        String query() {
            return query;
        }

        String contentType() {
            return contentType;
        }

        byte[] body() {
            return body;
        }

        @Override
        public void close() {
            server.stop(0);
        }
    }
}
