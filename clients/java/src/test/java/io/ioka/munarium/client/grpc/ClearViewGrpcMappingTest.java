// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.grpc;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;

import io.ioka.munarium.client.model.Json;
import io.ioka.munarium.client.model.SessionsApi;
import java.util.List;
import mmp.v1.Common;
import mmp.v1.Retrieval;
import mmp.v1.Session;
import org.junit.jupiter.api.Test;

/**
 * The typed-gRPC mapping of the ClearView C1 {@code coverage} and R1
 * {@code execution} members: proto3 absence (unset message, empty string,
 * unset {@code optional}) maps to the same nulls the REST twin decodes.
 */
class ClearViewGrpcMappingTest {

    private static final String REST_COVERAGE = "{\"policy\":\"report\",\"complete\":false,"
            + "\"members\":[{\"collection\":\"contracts\",\"stage\":\"search\","
            + "\"outcome\":\"complete\",\"hits\":3},"
            + "{\"collection\":\"memos\",\"stage\":\"search\",\"outcome\":\"empty\",\"hits\":0},"
            + "{\"collection\":\"letters\",\"stage\":\"probe\",\"outcome\":\"unavailable\","
            + "\"reason\":\"no_active_index\"}],"
            + "\"withheld_claims\":[\"absence\",\"exhaustive_coverage\"]}";

    @Test
    void coverageMapsLikeTheRestTwin() throws Exception {
        var resp = Session.TurnResponse.newBuilder()
                .setSessionId("s-1")
                .setOrdinal(1)
                .setCoverage(Session.TurnCoverage.newBuilder()
                        .setPolicy("report")
                        .setComplete(false)
                        .addMembers(Session.CollectionOutcome.newBuilder()
                                .setCollection("contracts").setStage("search")
                                .setOutcome("complete").setHits(3))
                        .addMembers(Session.CollectionOutcome.newBuilder()
                                .setCollection("memos").setStage("search")
                                .setOutcome("empty").setHits(0))
                        .addMembers(Session.CollectionOutcome.newBuilder()
                                .setCollection("letters").setStage("probe")
                                .setOutcome("unavailable").setReason("no_active_index"))
                        .addAllWithheldClaims(List.of("absence", "exhaustive_coverage")))
                .build();
        var grpc = GrpcTransport.turnResult(resp).coverage();
        assertNotNull(grpc);
        var rest = Json.MAPPER.readValue(REST_COVERAGE, SessionsApi.TurnCoverage.class);
        assertEquals(rest, grpc);
        assertEquals(0, grpc.members().get(1).hits(), "a measured zero stays zero");
        assertNull(grpc.members().get(2).hits(), "an unset optional stays unknown");
        assertNull(grpc.members().get(0).layer(), "an empty proto3 string is absence");
    }

    @Test
    void aTurnWithoutCoverageMapsToNull() {
        var resp = Session.TurnResponse.newBuilder().setSessionId("s-1").build();
        assertNull(GrpcTransport.turnResult(resp).coverage());
    }

    @Test
    void executionMapsUnsetLegMembersToNull() {
        var envelope = Common.ProvenanceEnvelope.newBuilder()
                .setIndexVersion("iv-1").setEventWatermark(9).build();
        var resp = Retrieval.HybridSearchResponse.newBuilder()
                .setEnvelope(envelope)
                .setExecution(Retrieval.SearchExecution.newBuilder()
                        .setVersion(1).setEngine("postgresql").setLexicalDomain("ts_rank")
                        .addLegs(Retrieval.SearchLegExecution.newBuilder()
                                .setLeg("lexical").setCandidateLimit(50).setFetched(0))
                        .addLegs(Retrieval.SearchLegExecution.newBuilder().setLeg("vector")))
                .build();
        var e = GrpcTransport.searchResult(resp).execution();
        assertNotNull(e);
        assertEquals("postgresql", e.engine());
        var lexical = e.legs().get(0);
        assertEquals(50L, lexical.candidateLimit());
        assertEquals(0L, lexical.fetched());
        assertNull(lexical.exhausted());
        var vector = e.legs().get(1);
        assertNull(vector.candidateLimit());
        assertNull(vector.fetched());
        assertNull(e.stages(), "the typed gRPC message carries no inference stages");

        var plain = GrpcTransport.searchResult(
                Retrieval.HybridSearchResponse.newBuilder().setEnvelope(envelope).build());
        assertNull(plain.execution());
        assertFalse(plain.hits().iterator().hasNext());
    }
}
