// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.rest;

import static java.nio.charset.StandardCharsets.UTF_8;
import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.sun.net.httpserver.HttpServer;
import io.ioka.munarium.client.MunariumClientOptions;
import io.ioka.munarium.client.model.Json;
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
import org.junit.jupiter.api.Test;

/**
 * The S-3.5 evidence-hierarchy surface, and its governing invariant: a caller
 * who does not use a research profile must see byte-identical request and
 * response behaviour. The wire assertions run against a loopback
 * {@link HttpServer} that records what it was sent, so they are about the
 * bytes that leave the process rather than about an intermediate map.
 */
class EvidenceHierarchyTest {

    private static final String LEGACY_TURN = "{\"session_id\":\"s-1\",\"ordinal\":1,"
            + "\"collections_searched\":[\"policies\"],\"skipped\":[],\"hits\":[],"
            + "\"envelopes\":[]}";

    // -- the invariant: an unprofiled turn's request bytes are unchanged -----

    @Test
    void legacyTurnRequestGainsNoResearchProfileKey() throws Exception {
        try (var server = new Recorder(LEGACY_TURN);
                var transport = transport(server)) {
            transport.turn("s-1", Params.TurnOptions.of("vacation policy"));
            assertEquals("{\"query\":\"vacation policy\"}", new String(server.body(), UTF_8),
                    "a turn without a research profile must send the bytes it always sent");
        }
    }

    @Test
    void legacyTurnRequestWithEveryOtherOptionIsAlsoUnchanged() throws Exception {
        try (var server = new Recorder(LEGACY_TURN);
                var transport = transport(server)) {
            transport.turn("s-1", Params.TurnOptions.of("vacation policy")
                    .withCompletion(SessionsApi.ModelOverride.tier("fast")));
            assertEquals(
                    "{\"query\":\"vacation policy\",\"complete\":true,"
                            + "\"model_override\":{\"tier\":\"fast\"}}",
                    new String(server.body(), UTF_8));
        }
    }

    @Test
    void researchProfileIsSentWhenSet() throws Exception {
        try (var server = new Recorder(LEGACY_TURN);
                var transport = transport(server)) {
            transport.turn("s-1",
                    Params.TurnOptions.of("late filings").withResearchProfile("regulatory"));
            assertEquals(
                    "{\"query\":\"late filings\",\"research_profile\":\"regulatory\"}",
                    new String(server.body(), UTF_8));
        }
    }

    // -- the invariant: an unprofiled turn's response is unchanged -----------

    @Test
    void legacyTurnResponseHasNoHierarchyAndRoundTripsWithoutTheKey() throws Exception {
        SessionsApi.TurnResult result;
        try (var server = new Recorder(LEGACY_TURN);
                var transport = transport(server)) {
            result = transport.turn("s-1", Params.TurnOptions.of("vacation policy"));
        }
        assertNull(result.hierarchy(), "a legacy turn carries no hierarchy decision");
        String reserialized = Json.MAPPER.writeValueAsString(result);
        assertFalse(reserialized.contains("hierarchy"),
                "re-serializing a legacy turn must not grow a key: " + reserialized);
    }

    @Test
    void hierarchyDecodesWhenAProfileRan() throws Exception {
        String body = "{\"session_id\":\"s-1\",\"ordinal\":2,\"collections_searched\":[],"
                + "\"skipped\":[],\"hits\":[],\"envelopes\":[],"
                + "\"hierarchy\":{\"profile\":\"regulatory\",\"intent_kind\":\"enumerate\","
                + "\"intent_explicit\":true,\"completeness_available\":true,"
                + "\"disclosed_conflicts\":2,\"conflicts_policy\":\"disclose\","
                + "\"layers\":[{\"layer\":\"register\",\"role\":\"controlling\","
                + "\"requirement\":\"required\",\"block\":\"complete_table\","
                + "\"evidence_id\":\"ev-7\",\"supports_completeness\":true,\"elapsed_ms\":41},"
                + "{\"layer\":\"documents\",\"role\":\"supporting\",\"requirement\":\"optional\","
                + "\"block\":\"refusal\",\"supports_completeness\":false,"
                + "\"refusal_code\":\"evidence-expired\",\"elapsed_ms\":9}]}}";
        SessionsApi.TurnResult result;
        try (var server = new Recorder(body);
                var transport = transport(server)) {
            result = transport.turn("s-1",
                    Params.TurnOptions.of("all filings").withResearchProfile("regulatory"));
        }
        var decision = result.hierarchy();
        assertNotNull(decision);
        assertEquals("regulatory", decision.profile());
        assertEquals("enumerate", decision.intentKind());
        assertTrue(decision.intentExplicit());
        assertTrue(decision.completenessAvailable());
        assertEquals(2, decision.disclosedConflicts());
        assertEquals("disclose", decision.conflictsPolicy());
        assertEquals(2, decision.layers().size());

        var controlling = decision.layers().get(0);
        assertEquals("register", controlling.layer());
        assertEquals("controlling", controlling.role());
        assertEquals("required", controlling.requirement());
        assertEquals("complete_table", controlling.block());
        assertEquals("ev-7", controlling.evidenceId());
        assertTrue(controlling.supportsCompleteness());
        assertNull(controlling.refusalCode());
        assertEquals(41L, controlling.elapsedMs());

        var refused = decision.layers().get(1);
        assertEquals("refusal", refused.block());
        assertEquals("evidence-expired", refused.refusalCode());
        assertNull(refused.evidenceId(), "an absent evidence_id must not read as an empty id");
        assertFalse(refused.supportsCompleteness());
    }

    // -- SSE: the six appended stages, and forward compatibility ------------

    @Test
    void hierarchyStagesDecodeAndAnUnknownStageDoesNotThrow() {
        String stream = sse("progress", "{\"stage\":\"profile\",\"profile\":\"regulatory\","
                        + "\"layers\":[\"register\",\"documents\"],\"intent_kind\":\"enumerate\","
                        + "\"intent_explicit\":false}")
                + sse("progress", "{\"stage\":\"layer_start\",\"layer\":\"register\","
                        + "\"role\":\"controlling\",\"requirement\":\"required\"}")
                + sse("progress", "{\"stage\":\"layer_source\",\"layer\":\"register\","
                        + "\"source\":\"filings@1\",\"provider\":\"matrix\"}")
                + sse("progress", "{\"stage\":\"layer_complete\",\"layer\":\"register\","
                        + "\"block\":\"complete_table\",\"supports_completeness\":true,"
                        + "\"elapsed_ms\":41}")
                + sse("progress", "{\"stage\":\"verify\",\"attempt\":0,\"checks\":[\"quotes\"],"
                        + "\"violations\":0,\"layer\":\"register\"}")
                + sse("progress", "{\"stage\":\"coverage\",\"completeness_available\":true,"
                        + "\"disclosed_conflicts\":2}")
                + sse("progress", "{\"stage\":\"compose\",\"layers_used\":2,"
                        + "\"context_chars\":8192,\"layers_dropped\":[\"documents\"]}")
                // A stage this build cannot name: progress is informational,
                // so it must flow through rather than break a paid turn.
                + sse("progress", "{\"stage\":\"telepathy\",\"unheard_of\":true}")
                + sse("done", LEGACY_TURN);

        var seen = new ArrayList<SessionsApi.TurnProgress>();
        SessionsApi.TurnResult result;
        try (var transport = offlineTransport()) {
            result = transport.readTurnStream(
                    new ByteArrayInputStream(stream.getBytes(UTF_8)),
                    seen::add,
                    Duration.ofSeconds(5));
        }
        assertEquals("s-1", result.sessionId());
        assertEquals(
                List.of("profile", "layer_start", "layer_source", "layer_complete", "verify",
                        "coverage", "compose", "telepathy"),
                seen.stream().map(SessionsApi.TurnProgress::stage).toList());

        var profile = seen.get(0);
        assertEquals("regulatory", profile.profile());
        assertEquals(List.of("register", "documents"), profile.layers());
        assertEquals("enumerate", profile.intentKind());
        assertEquals(Boolean.FALSE, profile.intentExplicit());

        var start = seen.get(1);
        assertEquals("register", start.layer());
        assertEquals("controlling", start.role());
        assertEquals("required", start.requirement());

        var source = seen.get(2);
        assertEquals("filings@1", source.source());
        assertEquals("matrix", source.provider());

        var complete = seen.get(3);
        assertEquals("complete_table", complete.block());
        assertEquals(Boolean.TRUE, complete.supportsCompleteness());
        assertNull(complete.refusalCode());
        assertEquals(41L, complete.elapsedMs());

        // The legacy `verify` stage gained an optional layer; its existing
        // members must still land where callers already read them.
        var verify = seen.get(4);
        assertEquals("register", verify.layer());
        assertEquals(List.of("quotes"), verify.checks());
        assertEquals(0, verify.violations());

        var coverage = seen.get(5);
        assertEquals(Boolean.TRUE, coverage.completenessAvailable());
        assertEquals(2, coverage.disclosedConflicts());

        var compose = seen.get(6);
        assertEquals(2, compose.layersUsed());
        assertEquals(8192, compose.contextChars());
        assertEquals(List.of("documents"), compose.layersDropped());

        // The unknown stage decodes to its name and nothing else — no throw,
        // and nothing invented.
        assertNull(seen.get(7).layer());
    }

    @Test
    void legacyProgressStagesAreUnaffectedByTheNewMembers() {
        String stream = sse("progress", "{\"stage\":\"retrieval\",\"collection\":\"policies\","
                        + "\"hits\":3,\"skipped\":false}")
                + sse("progress", "{\"stage\":\"verify\",\"attempt\":1,"
                        + "\"checks\":[\"quotes\",\"citations\"],\"violations\":2}")
                + sse("done", LEGACY_TURN);
        var seen = new ArrayList<SessionsApi.TurnProgress>();
        try (var transport = offlineTransport()) {
            transport.readTurnStream(
                    new ByteArrayInputStream(stream.getBytes(UTF_8)), seen::add,
                    Duration.ofSeconds(5));
        }
        assertEquals("policies", seen.get(0).collection());
        assertEquals(3, seen.get(0).hits());
        assertEquals(Boolean.FALSE, seen.get(0).skipped());
        assertEquals(2, seen.get(1).violations());
        assertNull(seen.get(1).layer(), "a legacy verify event carries no layer");
    }

    // -- the two new management reports -------------------------------------

    @Test
    void evidenceReportSendsTheWindowAndDecodes() throws Exception {
        String body = "{\"window\":\"7d\",\"hierarchy_turns\":120,\"legacy_turns\":8,"
                + "\"completeness_available\":97,\"layers\":[{\"profile\":\"regulatory\","
                + "\"layer\":\"register\",\"turns\":120,\"refusals\":11,\"complete\":97,"
                + "\"refusal_codes\":[\"matrix-unavailable\",\"evidence-expired\"],"
                + "\"p50_ms\":41,\"p95_ms\":388}]}";
        try (var server = new Recorder(body);
                var transport = transport(server)) {
            var report = transport.evidenceReport("7d");
            assertEquals("/v1/reports/evidence", server.path());
            assertEquals("window=7d", server.query());
            assertEquals("7d", report.window());
            assertEquals(120L, report.hierarchyTurns());
            assertEquals(8L, report.legacyTurns());
            assertEquals(97L, report.completenessAvailable());
            assertEquals(1, report.layers().size());
            var layer = report.layers().get(0);
            assertEquals("regulatory", layer.profile());
            assertEquals("register", layer.layer());
            assertEquals(120L, layer.turns());
            assertEquals(11L, layer.refusals());
            assertEquals(97L, layer.complete());
            assertEquals(List.of("matrix-unavailable", "evidence-expired"), layer.refusalCodes());
            assertEquals(41L, layer.p50Ms());
            assertEquals(388L, layer.p95Ms());
        }
    }

    @Test
    void evidenceReportWithoutAWindowLeavesTheServerDefaultAlone() throws Exception {
        try (var server = new Recorder(
                        "{\"window\":\"24h\",\"hierarchy_turns\":0,\"legacy_turns\":0,"
                                + "\"completeness_available\":0,\"layers\":[]}");
                var transport = transport(server)) {
            assertEquals("24h", transport.evidenceReport(null).window());
            assertNull(server.query(), "a null window must not send an empty parameter");
        }
    }

    @Test
    void matrixReportDecodes() throws Exception {
        String body = "{\"configured\":true,\"circuit_open\":false,\"consecutive_failures\":3,"
                + "\"data_views\":[{\"runbook_ref\":\"ent-support@4\",\"name\":\"filings\","
                + "\"contract\":\"late_filings@1\",\"access_level\":2}]}";
        try (var server = new Recorder(body);
                var transport = transport(server)) {
            var report = transport.matrix();
            assertEquals("/v1/reports/matrix", server.path());
            assertNull(server.query());
            assertTrue(report.configured());
            assertFalse(report.circuitOpen());
            assertEquals(3L, report.consecutiveFailures());
            assertEquals(1, report.dataViews().size());
            var view = report.dataViews().get(0);
            assertEquals("ent-support@4", view.runbookRef());
            assertEquals("filings", view.name());
            assertEquals("late_filings@1", view.contract());
            assertEquals(2, view.accessLevel());
        }
    }

    // -- fixtures ------------------------------------------------------------

    private static RestTransport transport(Recorder server) {
        return new RestTransport(
                MunariumClientOptions.of(server.url()).withToken("t").withUid("u"));
    }

    /** For the readTurnStream seam, which never opens a connection. */
    private static RestTransport offlineTransport() {
        return new RestTransport(
                MunariumClientOptions.of("http://127.0.0.1:1").withToken("t").withUid("u"));
    }

    private static String sse(String event, String data) {
        return "event: " + event + "\ndata: " + data + "\n\n";
    }

    /**
     * A loopback server that records what it was sent and answers one canned
     * body. Deliberately not a stub of RestTransport's internals: the claim
     * under test is about the bytes that leave the process.
     */
    private static final class Recorder implements AutoCloseable {
        private final HttpServer server;
        private final byte[] response;
        private volatile String path;
        private volatile String query;
        private volatile byte[] body = new byte[0];

        Recorder(String response) throws IOException {
            this.response = response.getBytes(UTF_8);
            this.server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
            server.createContext("/", exchange -> {
                path = exchange.getRequestURI().getPath();
                query = exchange.getRequestURI().getQuery();
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

        String path() {
            return path;
        }

        String query() {
            return query;
        }

        byte[] body() {
            return body;
        }

        @Override
        public void close() {
            server.stop(0);
        }
    }

    @Test
    void selectionAndExpansionStagesDecodeTheirOwnFields() throws Exception {
        // Server-side since 2026-08-25; until 2026-08-29 these decoded with
        // only `stage` set and everything worth emitting dropped.
        var selection = Json.MAPPER.readValue(
                "{\"stage\":\"selection\",\"probed\":58,\"selected\":3,"
                        + "\"collections\":[\"letterbooks\",\"narratives\"]}",
                SessionsApi.TurnProgress.class);
        assertEquals("selection", selection.stage());
        assertEquals(58, selection.probed());
        assertEquals(3, selection.selected());
        assertEquals(List.of("letterbooks", "narratives"), selection.collections());

        var expansion = Json.MAPPER.readValue(
                "{\"stage\":\"expansion\",\"provider\":\"anthropic\",\"model\":\"m\","
                        + "\"terms\":[\"vessel\"],\"input_tokens\":120,\"output_tokens\":8}",
                SessionsApi.TurnProgress.class);
        assertEquals(List.of("vessel"), expansion.terms());
        assertEquals("anthropic", expansion.provider());
        assertEquals(120L, expansion.inputTokens());
    }

    @Test
    void anEmptyExpansionTermListIsNotTheSameAsAnAbsentOne() {
        // Empty means the model ran and returned nothing usable, so the
        // original query searched alone. Null means no expansion step ran.
        // Collapsing them hides a paid call that bought nothing.
        var ranAndFoundNothing = assertDoesNotThrow(() -> Json.MAPPER.readValue(
                "{\"stage\":\"expansion\",\"provider\":\"a\",\"model\":\"m\",\"terms\":[]}",
                SessionsApi.TurnProgress.class));
        assertNotNull(ranAndFoundNothing.terms());
        assertTrue(ranAndFoundNothing.terms().isEmpty());

        var neverRan = assertDoesNotThrow(() -> Json.MAPPER.readValue(
                "{\"stage\":\"merge\",\"hits\":9}", SessionsApi.TurnProgress.class));
        assertNull(neverRan.terms());
    }
}
