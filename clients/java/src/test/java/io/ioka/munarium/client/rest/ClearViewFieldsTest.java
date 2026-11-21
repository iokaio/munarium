// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.rest;

import static java.nio.charset.StandardCharsets.UTF_8;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.sun.net.httpserver.HttpServer;
import io.ioka.munarium.client.MunariumClientOptions;
import io.ioka.munarium.client.errors.HeadConflictException;
import io.ioka.munarium.client.errors.InvalidInputException;
import io.ioka.munarium.client.errors.Problems;
import io.ioka.munarium.client.model.Json;
import io.ioka.munarium.client.model.Retrieval;
import io.ioka.munarium.client.model.SessionsApi;
import io.ioka.munarium.client.planes.Params;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.InetSocketAddress;
import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

/**
 * The Server 1.5 (ClearView) wire fields on the typed REST facade: C1
 * {@code collection_availability}/{@code coverage}, R1
 * {@code disclose_execution}/{@code execution}, the IR-3 turn
 * {@code execution} and R11 {@code retryable}/{@code retry_action}. Each request option leaves the
 * process only when set; each response member is optional, so a v1.4 body
 * still decodes.
 */
class ClearViewFieldsTest {

    private static final String LEGACY_TURN = "{\"session_id\":\"s-1\",\"ordinal\":1,"
            + "\"collections_searched\":[\"contracts\",\"letters\"],\"skipped\":[],\"hits\":[],"
            + "\"envelopes\":[]}";

    private static final String COVERAGE = "{\"policy\":\"report\",\"complete\":false,"
            + "\"members\":[{\"collection\":\"contracts\",\"stage\":\"search\","
            + "\"outcome\":\"complete\",\"hits\":3},"
            + "{\"collection\":\"memos\",\"stage\":\"search\",\"outcome\":\"empty\",\"hits\":0},"
            + "{\"collection\":\"letters\",\"stage\":\"probe\",\"outcome\":\"unavailable\","
            + "\"reason\":\"no_active_index\"}],"
            + "\"withheld_claims\":[\"absence\",\"exhaustive_coverage\"]}";

    private static final String COVERED_TURN =
            LEGACY_TURN.substring(0, LEGACY_TURN.length() - 1) + ",\"coverage\":" + COVERAGE + "}";

    private static final String ENVELOPE = "{\"chunk_ids\":[],\"source_ids\":[],"
            + "\"source_paths\":[],\"source_content_hashes\":[],\"index_version\":\"iv-1\","
            + "\"event_watermark\":9}";

    private static final String LEGACY_SEARCH = "{\"hits\":[],\"envelope\":" + ENVELOPE + "}";

    private static final String DISCLOSED_SEARCH = "{\"hits\":[],\"envelope\":" + ENVELOPE
            + ",\"execution\":{\"version\":1,\"engine\":\"postgresql\","
            + "\"lexical_domain\":\"ts_rank\",\"legs\":["
            + "{\"leg\":\"lexical\",\"candidate_limit\":50,\"fetched\":12,\"exhausted\":true},"
            + "{\"leg\":\"vector\",\"candidate_limit\":50,\"fetched\":null,\"exhausted\":null}],"
            + "\"stages\":[{\"stage\":\"reranking\",\"policy\":\"prefer\",\"policy_revision\":4,"
            + "\"applied\":false,\"state\":\"unavailable\",\"fallback\":\"fused_order\"}]}}";

    // -- C1: the request option ----------------------------------------------

    @Test
    void turnSendsCollectionAvailabilityOnlyWhenSelected() throws Exception {
        try (var server = new Recorder(LEGACY_TURN);
                var transport = transport(server)) {
            transport.turn("s-1", Params.TurnOptions.of("q"));
            assertEquals("{\"query\":\"q\"}", new String(server.body(), UTF_8),
                    "a turn without a policy must send the v1.4 bytes");
            transport.turn("s-1", Params.TurnOptions.of("q").withCollectionAvailability(
                    Params.TurnOptions.COLLECTION_AVAILABILITY_REQUIRE));
            assertEquals("{\"query\":\"q\",\"collection_availability\":\"require\"}",
                    new String(server.body(), UTF_8));
        }
    }

    @Test
    void theFiveComponentTurnOptionsConstructorStillMeansNoPolicy() {
        var o = new Params.TurnOptions("q", null, null, null, null);
        assertNull(o.collectionAvailability());
        assertEquals("report",
                o.withCollectionAvailability("report").withResearchProfile("p")
                        .collectionAvailability(),
                "withers carry the policy along");
    }

    // -- C1: the response member ---------------------------------------------

    @Test
    void turnDecodesCoverage() throws Exception {
        SessionsApi.TurnResult result;
        try (var server = new Recorder(COVERED_TURN);
                var transport = transport(server)) {
            result = transport.turn("s-1",
                    Params.TurnOptions.of("q").withCollectionAvailability("report"));
        }
        assertCoverage(result.coverage());
    }

    @Test
    void aV14TurnWithoutCoverageDecodesToNullAndReserializesWithoutTheKey() throws Exception {
        SessionsApi.TurnResult result;
        try (var server = new Recorder(LEGACY_TURN);
                var transport = transport(server)) {
            result = transport.turn("s-1", Params.TurnOptions.of("q"));
        }
        assertNull(result.coverage());
        assertFalse(Json.MAPPER.writeValueAsString(result).contains("coverage"));
    }

    @Test
    void coverageRoundTripsThroughJson() throws Exception {
        var result = Json.MAPPER.readValue(COVERED_TURN, SessionsApi.TurnResult.class);
        var again = Json.MAPPER.readValue(
                Json.MAPPER.writeValueAsString(result), SessionsApi.TurnResult.class);
        assertEquals(result.coverage(), again.coverage());
        assertCoverage(again.coverage());
    }

    @Test
    void sseDoneCarriesCoverageAndMemberProgressKeepsOutcomeAndReason() {
        String stream = sse("progress", "{\"stage\":\"retrieval\",\"collection\":\"letters\","
                        + "\"hits\":0,\"skipped\":false,\"outcome\":\"unavailable\","
                        + "\"reason\":\"engine_unavailable\"}")
                + sse("progress", "{\"stage\":\"retrieval\",\"collection\":\"contracts\","
                        + "\"hits\":3,\"skipped\":false}")
                + sse("done", COVERED_TURN);
        var seen = new ArrayList<SessionsApi.TurnProgress>();
        SessionsApi.TurnResult result;
        try (var transport = offlineTransport()) {
            result = transport.readTurnStream(
                    new ByteArrayInputStream(stream.getBytes(UTF_8)), seen::add,
                    Duration.ofSeconds(5));
        }
        assertCoverage(result.coverage());
        assertEquals("unavailable", seen.get(0).outcome());
        assertEquals("engine_unavailable", seen.get(0).reason());
        assertNull(seen.get(1).outcome(), "an event without a policy carries no outcome");
        assertNull(seen.get(1).reason());
    }

    static void assertCoverage(SessionsApi.TurnCoverage c) {
        assertNotNull(c);
        assertEquals("report", c.policy());
        assertFalse(c.complete());
        assertEquals(List.of("absence", "exhaustive_coverage"), c.withheldClaims());
        assertEquals(3, c.members().size());
        var complete = c.members().get(0);
        assertEquals("contracts", complete.collection());
        assertEquals("complete", complete.outcome());
        assertEquals(3, complete.hits());
        assertNull(complete.layer());
        assertNull(complete.reason());
        // An authorized empty member is a successful outcome with a zero count.
        var empty = c.members().get(1);
        assertEquals("empty", empty.outcome());
        assertEquals(0, empty.hits());
        var unavailable = c.members().get(2);
        assertEquals("probe", unavailable.stage());
        assertEquals("unavailable", unavailable.outcome());
        assertNull(unavailable.hits(), "an unavailable member has no count, not zero");
        assertEquals("no_active_index", unavailable.reason());
    }

    // -- R1: disclose_execution / execution ----------------------------------

    @Test
    void searchSendsDiscloseExecutionOnlyWhenSetAndDecodesExecution() throws Exception {
        try (var server = new Recorder(DISCLOSED_SEARCH);
                var transport = transport(server)) {
            transport.search(Params.SearchQuery.of("q", "docs"));
            assertFalse(new String(server.body(), UTF_8).contains("disclose_execution"));
            var result = transport.search(
                    Params.SearchQuery.of("q", "docs").withDiscloseExecution(true));
            assertTrue(new String(server.body(), UTF_8).contains("\"disclose_execution\":true"));
            Retrieval.SearchExecution e = result.execution();
            assertNotNull(e);
            assertEquals(1, e.version());
            assertEquals("postgresql", e.engine());
            assertEquals("ts_rank", e.lexicalDomain());
            var lexical = e.legs().get(0);
            assertEquals(12L, lexical.fetched());
            assertEquals(Boolean.TRUE, lexical.exhausted());
            var vector = e.legs().get(1);
            assertNull(vector.fetched(), "null is not measured: unknown, never zero");
            assertNull(vector.exhausted());
            var stage = e.stages().get(0);
            assertEquals("fused_order", stage.fallback());
            assertEquals(4L, stage.policyRevision());
            assertNull(stage.reason());
        }
    }

    @Test
    void aV14SearchWithoutExecutionDecodesToNull() throws Exception {
        try (var server = new Recorder(LEGACY_SEARCH);
                var transport = transport(server)) {
            var result = transport.search(Params.SearchQuery.of("q", "docs"));
            assertNull(result.execution());
            assertFalse(Json.MAPPER.writeValueAsString(result).contains("execution"));
        }
    }

    // -- IR-3: the multi-collection turn execution ----------------------------

    private static final String TURN_EXECUTION = "{\"version\":1,\"stages\":["
            + "{\"stage\":\"reranking\",\"policy\":\"require\",\"policy_revision\":2,"
            + "\"applied\":true,\"state\":\"ready\",\"identity\":\"rr-a\",\"scored\":8,"
            + "\"collection\":\"contracts\"},"
            + "{\"stage\":\"reranking\",\"policy\":\"prefer\",\"policy_revision\":5,"
            + "\"applied\":false,\"state\":\"unavailable\",\"fallback\":\"fused_order\","
            + "\"collection\":\"letters\"}]}";

    private static final String EXECUTED_TURN =
            COVERED_TURN.substring(0, COVERED_TURN.length() - 1)
                    + ",\"execution\":" + TURN_EXECUTION + "}";

    static void assertTurnExecution(Retrieval.ReadExecution e) {
        assertNotNull(e);
        assertEquals(1, e.version());
        assertEquals(2, e.stages().size());
        var contracts = e.stages().get(0);
        assertEquals("contracts", contracts.collection());
        assertTrue(contracts.applied());
        assertEquals(8L, contracts.scored());
        assertEquals("rr-a", contracts.identity());
        assertNull(contracts.fallback());
        var letters = e.stages().get(1);
        assertEquals("letters", letters.collection());
        assertEquals("fused_order", letters.fallback());
    }

    @Test
    void turnDecodesTheMultiCollectionExecution() throws Exception {
        SessionsApi.TurnResult result;
        try (var server = new Recorder(EXECUTED_TURN);
                var transport = transport(server)) {
            result = transport.turn("s-1", Params.TurnOptions.of("q"));
        }
        assertTurnExecution(result.execution());
        assertCoverage(result.coverage());
        var again = Json.MAPPER.readValue(
                Json.MAPPER.writeValueAsString(result), SessionsApi.TurnResult.class);
        assertEquals(result.execution(), again.execution());
    }

    @Test
    void aTurnWithoutExecutionOrWithNullDecodesToNullAndOmitsTheKey() throws Exception {
        String nullExecution = LEGACY_TURN.substring(0, LEGACY_TURN.length() - 1)
                + ",\"execution\":null}";
        for (String body : List.of(LEGACY_TURN, COVERED_TURN, nullExecution)) {
            var result = Json.MAPPER.readValue(body, SessionsApi.TurnResult.class);
            assertNull(result.execution(), body);
            assertFalse(Json.MAPPER.writeValueAsString(result).contains("execution"), body);
        }
    }

    @Test
    void sseDoneCarriesTheMultiCollectionExecution() {
        SessionsApi.TurnResult result;
        try (var transport = offlineTransport()) {
            result = transport.readTurnStream(
                    new ByteArrayInputStream(sse("done", EXECUTED_TURN).getBytes(UTF_8)),
                    p -> { }, Duration.ofSeconds(5));
        }
        assertTurnExecution(result.execution());
    }

    @Test
    void theOlderTurnResultConstructorsLeaveExecutionNull() {
        var v14 = new SessionsApi.TurnResult("s-1", 1, List.of(), List.of(), List.of(), List.of(),
                null, null);
        var covered = new SessionsApi.TurnResult("s-1", 1, List.of(), List.of(), List.of(),
                List.of(), null, null, null);
        assertNull(v14.execution());
        assertNull(covered.execution());
    }

    @Test
    void aSingleSearchStageDecodesWithAndWithoutCollection() throws Exception {
        var plain = Json.MAPPER.readValue(DISCLOSED_SEARCH, Retrieval.SearchResult.class);
        assertNull(plain.execution().stages().get(0).collection());
        assertFalse(Json.MAPPER.writeValueAsString(plain).contains("collection"));
        var named = Json.MAPPER.readValue(
                DISCLOSED_SEARCH.replace("\"fallback\":\"fused_order\"",
                        "\"fallback\":\"fused_order\",\"collection\":\"contracts\""),
                Retrieval.SearchResult.class);
        assertEquals("contracts", named.execution().stages().get(0).collection());
        var nineComponents = new Retrieval.SearchStage("embedding", "require", 1, true, "ready",
                null, null, null, null);
        assertNull(nineComponents.collection());
    }

    // -- R11: retryable / retry_action ---------------------------------------

    @Test
    void problemJsonCarriesRetryGuidance() throws Exception {
        var body = Json.MAPPER.readTree("{\"type\":\"https://munarium.ioka.io/problems/"
                + "head-conflict\",\"title\":\"t\",\"status\":409,\"detail\":\"d\","
                + "\"expected\":1,\"actual\":2,\"retryable\":false,\"retry_action\":\"reread\"}");
        var e = assertInstanceOf(HeadConflictException.class,
                Problems.fromProblemJson(409, body, null));
        assertEquals(2, e.actual());
        assertEquals(Boolean.FALSE, e.retryable());
        assertEquals("reread", e.retryAction());
    }

    @Test
    void aV14ProblemCarriesNoRetryGuidance() throws Exception {
        var body = Json.MAPPER.readTree("{\"type\":\"https://munarium.ioka.io/problems/"
                + "invalid-input\",\"title\":\"t\",\"status\":400,\"detail\":\"d\"}");
        var e = assertInstanceOf(InvalidInputException.class,
                Problems.fromProblemJson(400, body, null));
        assertNull(e.retryable());
        assertNull(e.retryAction());
    }

    @Test
    void grpcErrorInfoMetadataCarriesRetryGuidance() {
        var e = Problems.fromGrpcInfo("overloaded", "shed",
                Map.of("retryable", "true", "retry_action", "repeat"));
        assertEquals(Boolean.TRUE, e.retryable());
        assertEquals("repeat", e.retryAction());
        var bare = Problems.fromGrpcInfo("invalid-input", "bad", Map.of());
        assertNull(bare.retryable());
        assertNull(bare.retryAction());
    }

    // -- fixtures ------------------------------------------------------------

    private static RestTransport transport(Recorder server) {
        return new RestTransport(
                MunariumClientOptions.of(server.url()).withToken("t").withUid("u"));
    }

    private static RestTransport offlineTransport() {
        return new RestTransport(
                MunariumClientOptions.of("http://127.0.0.1:1").withToken("t").withUid("u"));
    }

    private static String sse(String event, String data) {
        return "event: " + event + "\ndata: " + data + "\n\n";
    }

    /** A loopback server that records the last body it was sent. */
    private static final class Recorder implements AutoCloseable {
        private final HttpServer server;
        private final byte[] response;
        private volatile byte[] body = new byte[0];

        Recorder(String response) throws IOException {
            this.response = response.getBytes(UTF_8);
            this.server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
            server.createContext("/", exchange -> {
                try (InputStream in = exchange.getRequestBody()) {
                    body = in.readAllBytes();
                }
                exchange.getResponseHeaders().add("content-type", "application/json");
                exchange.sendResponseHeaders(200, this.response.length);
                try (OutputStream out = exchange.getResponseBody()) {
                    out.write(this.response);
                }
            });
            server.start();
        }

        String url() {
            return "http://127.0.0.1:" + server.getAddress().getPort();
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
