// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.rest;

import static java.nio.charset.StandardCharsets.UTF_8;
import static org.junit.jupiter.api.Assertions.*;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.sun.net.httpserver.HttpServer;
import io.ioka.munarium.client.MunariumClientOptions;
import io.ioka.munarium.client.ServerApiTransport;
import io.ioka.munarium.client.errors.UnexpectedServerException;
import io.ioka.munarium.client.grpc.GrpcTransport;
import io.ioka.munarium.client.model.Json;
import io.ioka.munarium.client.model.Ledger;
import io.ioka.munarium.client.model.Providers;
import java.lang.reflect.InvocationTargetException;
import java.net.InetSocketAddress;
import java.math.BigInteger;
import java.util.Map;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

class WireCompatibilityTest {
    @ParameterizedTest
    @ValueSource(longs = {0, 9007199254740991L, 9007199254740992L,
            9007199254740993L, Long.MAX_VALUE})
    void restAndProtobufPreserveExactSequences(long value) throws Exception {
        var body = "{\"head_seq\":" + value + ",\"as_of_seq\":" + value
                + ",\"facts\":[],\"future\":{\"n\":9223372036854775807}}";
        var page = Json.MAPPER.readValue(body, Ledger.FactsPage.class);
        assertEquals(value, page.headSeq());
        assertEquals(value, page.asOfSeq());
        assertEquals(value, Json.MAPPER.readTree(Json.MAPPER.writeValueAsBytes(page))
                .get("head_seq").longValue());
        var proto = mmp.v1.Ledger.Claim.newBuilder().setSeq(value)
                .setUnknownFields(com.google.protobuf.UnknownFieldSet.newBuilder()
                        .addField(127, com.google.protobuf.UnknownFieldSet.Field.newBuilder()
                                .addVarint(1).build()).build()).build();
        assertEquals(value, convertClaim(mmp.v1.Ledger.Claim.parseFrom(proto.toByteArray())).seq());
        withHead(body, transport -> assertEquals(value, transport.head("fixture")));
    }

    @ParameterizedTest
    @ValueSource(longs = {Long.MIN_VALUE, -1L})
    void grpcRejectsUint64ValuesThatDoNotFitTheTypedFacade(long bits) {
        var proto = mmp.v1.Ledger.Claim.newBuilder().setSeq(bits).build();
        var error = assertThrows(InvocationTargetException.class, () -> convertClaim(proto));
        assertInstanceOf(UnexpectedServerException.class, error.getCause());
    }

    @Test
    void grpcRejectsUnrepresentableHierarchyElapsedTime() throws Exception {
        var hierarchy = mmp.v1.Session.EvidenceHierarchyDecision.newBuilder()
                .addLayers(mmp.v1.Session.LayerOutcome.newBuilder().setElapsedMs(Long.MIN_VALUE)).build();
        var conversion = GrpcTransport.class.getDeclaredMethod(
                "hierarchy", mmp.v1.Session.EvidenceHierarchyDecision.class);
        conversion.setAccessible(true);
        var error = assertThrows(InvocationTargetException.class,
                () -> conversion.invoke(null, hierarchy));
        assertInstanceOf(UnexpectedServerException.class, error.getCause());
    }

    @ParameterizedTest
    @ValueSource(ints = {0, 999})
    void futureProtobufEnumsRemainConservative(int tag) throws Exception {
        var proto = mmp.v1.Ledger.Claim.newBuilder()
                .setStatusValue(tag).setProvenanceValue(tag).build();
        var claim = convertClaim(proto);
        assertEquals("disputed", claim.status());
        assertEquals("emergent", claim.provenance());
        var conversion = GrpcTransport.class.getDeclaredMethod("finding", mmp.v1.Common.GateFinding.class);
        conversion.setAccessible(true);
        var finding = (Ledger.GateFinding) conversion.invoke(null,
                mmp.v1.Common.GateFinding.newBuilder().setSeverityValue(tag).build());
        assertEquals("block", finding.severity());
    }

    private static Ledger.Claim convertClaim(mmp.v1.Ledger.Claim proto) throws Exception {
        var conversion = GrpcTransport.class.getDeclaredMethod("claim", mmp.v1.Ledger.Claim.class);
        conversion.setAccessible(true);
        return (Ledger.Claim) conversion.invoke(null, proto);
    }

    @ParameterizedTest
    @ValueSource(strings = {"-1", "9223372036854775808", "18446744073709551616",
            "1.5", "9007199254740992.0", "true", "\"12\"", "\"bad\"", "null"})
    void headRejectsInvalidOrUnrepresentableIntegerTokens(String value) throws Exception {
        withHead("{\"head_seq\":" + value + "}", transport ->
                assertThrows(UnexpectedServerException.class, () -> transport.head("fixture")));
    }

    @ParameterizedTest
    @ValueSource(strings = {"9223372036854775808", "1.5", "9007199254740992.0", "\"12\""})
    void typedRecordsRejectLossyIntegerConversions(String value) {
        assertThrows(JsonProcessingException.class, () -> Json.MAPPER.readValue(
                "{\"head_seq\":" + value + ",\"as_of_seq\":0,\"facts\":[]}", Ledger.FactsPage.class));
    }

    @Test
    void typedNullsRemainOmittedAndUnknownFieldsAreIgnored() throws Exception {
        var raw = "{\"subject\":\"fixture\",\"key\":\"key\",\"value\":\"value\",\"origin\":null,"
                + "\"future\":9223372036854775807}";
        var input = Json.MAPPER.readValue(raw, Ledger.ClaimInput.class);
        assertNull(input.origin());
        assertFalse(Json.MAPPER.readTree(Json.MAPPER.writeValueAsBytes(input)).has("origin"));
    }

    @ParameterizedTest
    @ValueSource(strings = {"9007199254740993", "9223372036854775807", "9223372036854775808",
            "18446744073709551615", "-9223372036854775808"})
    void fullApiJsonCarriesExactSignedAndUnsignedLimits(String decimal) throws Exception {
        var raw = Json.MAPPER.readTree("{\"n\":" + decimal + ",\"optional\":null}");
        var request = ServerApiTransport.ApiRequest.json(raw, Map.of());
        var proto = mmp.v1.ServerApi.ServerApiRequest.newBuilder()
                .setBody(com.google.protobuf.ByteString.copyFrom(request.body())).build();
        var response = new ServerApiTransport.ApiResponse(200, "application/json",
                mmp.v1.ServerApi.ServerApiRequest.parseFrom(proto.toByteArray()).getBody().toByteArray());
        assertEquals(new BigInteger(decimal), response.json().get("n").bigIntegerValue());
        assertTrue(response.json().get("optional").isNull());
        assertFalse(response.json().has("omitted"));
    }

    @ParameterizedTest
    @ValueSource(strings = {"", ",\"usage\":null",
            ",\"usage\":{\"quality\":\"future_quality\",\"input_tokens\":null}"})
    void previousAndAdditiveCurrentCompletionShapesRemainReadable(String additions) throws Exception {
        // Synthetic 1.1 baseline and additive current/future fields, not old binaries.
        var raw = "{\"text\":\"fictional answer\",\"stop_reason\":\"stop\","
                + "\"input_tokens\":9007199254740993,\"output_tokens\":1,"
                + "\"provider\":\"fixture\",\"model\":\"fixture\","
                + "\"future_optional\":{\"opaque_id\":\"id:with/slash+suffix\"}" + additions + "}";
        var typed = Json.MAPPER.readValue(raw, Providers.CompleteResult.class);
        assertEquals(9007199254740993L, typed.inputTokens());
        assertNull(typed.invocationEventId());
        var response = new ServerApiTransport.ApiResponse(200, "application/json", raw.getBytes(UTF_8));
        assertEquals("id:with/slash+suffix", response.json().get("future_optional").get("opaque_id").asText());
        assertEquals(!additions.isEmpty(), response.json().has("usage"));
    }

    @ParameterizedTest
    @ValueSource(strings = {"\"future_status\"", "\"\"", "null", "\"disputed\"", "\"accepted\""})
    void onlyExplicitAcceptedRestStatusCanReadAsUndisputed(String status) throws Exception {
        var claim = "{\"id\":\"fixture\",\"version_id\":\"v\",\"seq\":1,\"claim_type\":\"fact\","
                + "\"subject\":\"fixture\",\"key\":\"key\",\"value\":\"value\",\"normalized_text\":\"fixture\","
                + "\"provenance\":\"witnessed\",\"status\":" + status + "}";
        var outcome = Json.MAPPER.readValue("{\"claim\":" + claim + ",\"findings\":[],\"head_seq\":1}",
                Ledger.ClaimOutcome.class);
        assertEquals(!status.equals("\"accepted\""), outcome.isDisputed());
        var events = Json.MAPPER.readValue("{\"claims\":[" + claim + "],\"findings\":[],\"head_seq\":1}",
                Ledger.EventsOutcome.class);
        assertEquals(!status.equals("\"accepted\""), events.isDisputed());
    }

    private static void withHead(String body, java.util.function.Consumer<RestTransport> check)
            throws Exception {
        var server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/", exchange -> {
            byte[] bytes = body.getBytes(UTF_8);
            exchange.getResponseHeaders().set("Content-Type", "application/json");
            exchange.sendResponseHeaders(200, bytes.length);
            try (var out = exchange.getResponseBody()) { out.write(bytes); }
        });
        server.start();
        try (var transport = new RestTransport(MunariumClientOptions.of(
                "http://127.0.0.1:" + server.getAddress().getPort()))) {
            check.accept(transport);
        } finally {
            server.stop(0);
        }
    }
}
