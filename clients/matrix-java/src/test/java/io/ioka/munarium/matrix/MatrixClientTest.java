// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import org.junit.jupiter.api.Test;

/**
 * Conformance for the Java Matrix client — the offline tier.
 *
 * <p>These assert the response SHAPES this client claims to understand, driven
 * through a stub HTTP server that states the exact bytes the service would
 * have sent. They run everywhere and they are what catch a field rename in the
 * API.
 *
 * <p>There is no mock of Matrix's <i>semantics</i> here. A client test that
 * asserted what a refusal MEANS would be asserting its own opinion; these
 * assert only that the client reads what the service says.
 */
class MatrixClientTest {

    @Test
    void versionReportsLockstepFromTheServicesOwnWord() throws Exception {
        try (var stub = new StubMatrix(request -> {
            assertEquals("/version", request.path());
            return StubMatrix.Reply.json(200, """
                    {"version":"0.1.0","contract_version":"0.1.0","role":"all",
                     "server_version":"0.5.0","target_server_version":"0.5.0",
                     "server_compatibility":"exact","uptime_seconds":42}
                    """);
        }); var mx = stub.client()) {
            Version v = mx.version();
            assertTrue(v.lockstepOk());
            assertEquals("all", v.role());
            assertEquals(Long.valueOf(42), v.uptimeSeconds());
        }
    }

    @Test
    void aNonExactLockstepIsNotOk() throws Exception {
        try (var stub = new StubMatrix(request -> StubMatrix.Reply.json(200, """
                {"version":"0.1.0","contract_version":"0.1.0","role":"all",
                 "target_server_version":"0.5.0","server_compatibility":"minor_behind"}
                """)); var mx = stub.client()) {
            // The distinction the whole lockstep exists for: an id minted
            // against a server that does not agree on the contract may not
            // resolve there.
            assertFalse(mx.version().lockstepOk());
        }
    }

    @Test
    void applyPostsYamlAsYamlAndReportsUnchanged() throws Exception {
        try (var stub = new StubMatrix(request -> {
            assertEquals("text/yaml", request.headers().get("content-type"));
            assertTrue(request.body().contains("kind: DataSource"));
            return StubMatrix.Reply.json(
                    200, "{\"asset_ref\":\"crm@2\",\"kind\":\"DataSource\",\"unchanged\":true}");
        }); var mx = stub.client()) {
            ApplyOutcome outcome = mx.apply("kind: DataSource\n");
            assertEquals("crm@2", outcome.assetRef());
            // Re-applying identical bytes is ordinary GitOps, not an error.
            assertTrue(outcome.unchanged());
        }
    }

    @Test
    void aRefusalSurfacesItsClassAndCodeRatherThanProse() throws Exception {
        try (var stub = new StubMatrix(request -> StubMatrix.Reply.problem(429, """
                {"type":"https://munarium.ioka.io/problems/matrix/budget-exceeded",
                 "title":"exhausted","status":429,
                 "detail":"source 'crm' has 0 of 2 unit(s) left this hour",
                 "refusal":{"class":"exhausted","code":"budget_exceeded",
                            "message":"budget spent"}}
                """)); var mx = stub.client()) {
            MatrixException e =
                    assertThrows(MatrixException.class, () -> mx.verify("open-pipeline-by-region"));
            assertEquals("budget_exceeded", e.code());
            assertEquals("exhausted", e.refusalClass());
            // A caller deciding whether to retry must not be parsing prose.
            assertTrue(e.retryable());
        }
    }

    @Test
    void aDenialIsNotRetryable() throws Exception {
        try (var stub = new StubMatrix(request -> StubMatrix.Reply.problem(403, """
                {"title":"denied","status":403,"detail":"role 'ro' cannot execute commands",
                 "refusal":{"class":"denied","code":"policy_denied","message":"no"}}
                """)); var mx = stub.client()) {
            MatrixException e = assertThrows(MatrixException.class, () -> mx.sync("crm"));
            // Repeating a request against a door locked on purpose is not a retry.
            assertFalse(e.retryable());
            assertEquals("policy_denied", e.code());
        }
    }

    @Test
    void verifyReportsWhichQuestionMoved() throws Exception {
        try (var stub = new StubMatrix(request -> StubMatrix.Reply.json(200, """
                {"contract":"open-pipeline-by-region@3","passed":0,"failed":1,
                 "questions":[{"question":"What is the open pipeline by region?",
                               "ok":false,"rows":1,
                               "failures":["expected 3 rows, got 1"]}]}
                """)); var mx = stub.client()) {
            VerifyOutcome out = mx.verify("open-pipeline-by-region");
            // The call succeeded and the CONTRACT did not: different things.
            assertEquals(1, out.failed());
            assertEquals(List.of("expected 3 rows, got 1"), out.questions().get(0).failures());
        }
    }

    @Test
    void verifyViewFallsBackFromMetricViewToDataView() throws Exception {
        try (var stub = new StubMatrix(request -> {
            if (request.path().contains("metricviews")) {
                // Matrix's not-found problem carries no `refusal` object at
                // all, which is exactly the body this fallback must survive.
                return StubMatrix.Reply.problem(404,
                        "{\"type\":\"https://munarium.ioka.io/problems/matrix/not-found\","
                                + "\"title\":\"not found\",\"status\":404,"
                                + "\"detail\":\"MetricView 'pipeline-by-region'\"}");
            }
            return StubMatrix.Reply.json(200, """
                    {"contract":"pipeline-by-region@2","passed":1,"failed":0,
                     "fingerprint":"sha256:abc","questions":[]}
                    """);
        }); var mx = stub.client()) {
            VerifyOutcome out = mx.verifyView("pipeline-by-region");
            assertEquals("sha256:abc", out.fingerprint());
            assertEquals(
                    List.of("/v1/metricviews/pipeline-by-region/verify",
                            "/v1/dataviews/pipeline-by-region/verify"),
                    stub.paths());
        }
    }

    @Test
    void aTransportFailureIsUnavailableNotABareException() throws Exception {
        try (var mx = MatrixClient.of(StubMatrix.deadEndpoint())) {
            MatrixException e = assertThrows(MatrixException.class, mx::healthdata);
            assertEquals("unavailable", e.refusalClass());
            assertTrue(e.retryable());
            assertNull(e.status());
        }
    }

    @Test
    void healthzAnswersFalseRatherThanThrowing() throws Exception {
        try (var mx = MatrixClient.of(StubMatrix.deadEndpoint())) {
            assertFalse(mx.healthz());
        }
    }

    // -- shapes the Python sibling reads wrongly, pinned here ------------------

    @Test
    void listAssetsAsksForHistoryByTheNameTheServiceActuallyReads() throws Exception {
        try (var stub = new StubMatrix(request -> StubMatrix.Reply.json(200, """
                {"assets":[{"asset_ref":"crm@2","name":"crm","version":2,
                            "kind":"DataSource","created_at":"2026-08-29T00:00:00Z",
                            "source":null}]}
                """)); var mx = stub.client()) {
            List<AssetSummary> assets = mx.listAssets("datasources", true);
            assertEquals("crm@2", assets.get(0).assetRef());
            assertEquals(2, assets.get(0).version());
            // `all_versions` is the parameter Matrix deserializes. `all` is
            // dropped in silence, and a silently-latest-only listing looks
            // exactly like a registry that has no history.
            assertEquals("all_versions=true", stub.seen().get(0).query());
        }
    }

    @Test
    void aLatestOnlyListingSendsNoParameterAtAll() throws Exception {
        try (var stub = new StubMatrix(request -> StubMatrix.Reply.json(200, "{\"assets\":[]}"));
                var mx = stub.client()) {
            assertTrue(mx.listAssets("contracts").isEmpty());
            assertNull(stub.seen().get(0).query());
        }
    }

    @Test
    void promotionStatusReadsTheGatesWhereMatrixPutsThem() throws Exception {
        try (var stub = new StubMatrix(request -> StubMatrix.Reply.json(200, """
                {"mapping":"captable@1","mode":"authoritative","promoted":true,
                 "promoted_version":1,"decision_id":"CHG-9","authority_scopes":2,
                 "gates":{"identity_precision":1.0,"value_conformance":0.99,
                          "min_identity_precision":0.95,"min_value_conformance":0.99,
                          "observations":10,"run_id":"mrn-1"},
                 "latest_run":{"run_id":"mrn-1","state":"ok","observations":10,
                               "discrepancies":7,"ambiguous":0,"findings_filed":7,
                               "proposals":1,"ended_at":"2026-08-29T00:00:00Z"}}
                """)); var mx = stub.client()) {
            PromotionStatus status = mx.promotionStatus("captable");
            // Nested under `gates`, never at the top level. Read from the top
            // level these are null forever, which reads as "never measured".
            assertNotNull(status.gates());
            assertEquals(Double.valueOf(1.0), status.identityPrecision());
            assertEquals(Double.valueOf(0.99), status.valueConformance());
            assertEquals(Double.valueOf(0.95), status.gates().minIdentityPrecision());
            // The asset_ref the service returned, not the name we asked with.
            assertEquals("captable@1", status.mapping());
            assertEquals("ok", status.latestRun().state());
            assertEquals(7L, status.latestRun().findingsFiled());
        }
    }

    @Test
    void anInvalidAssetPutsAFindingArrayInRefusalAndTheDecoderSurvivesIt() throws Exception {
        // The single most common way to get a 422 out of this service: apply
        // an asset that fails validation. Matrix reuses the `refusal` key for
        // an ARRAY of findings there, so a decoder that assumed an object
        // would blow up on the ordinary case.
        try (var stub = new StubMatrix(request -> StubMatrix.Reply.problem(422, """
                {"type":"https://munarium.ioka.io/problems/matrix/asset-invalid",
                 "title":"asset failed validation","status":422,
                 "detail":"1 error finding(s); nothing was applied",
                 "refusal":[{"code":"connection.secret-literal",
                             "path":"spec.connection.password",
                             "message":"looks like a literal secret"}]}
                """)); var mx = stub.client()) {
            MatrixException e = assertThrows(MatrixException.class, () -> mx.apply("kind: DataSource\n"));
            assertEquals(Integer.valueOf(422), e.status());
            assertEquals("1 error finding(s); nothing was applied", e.detail());
            // No refusal OBJECT was sent, so there is no class to report — and
            // reporting one anyway would be an invention.
            assertNull(e.refusalClass());
            assertFalse(e.retryable());
        }
    }

    @Test
    void validateReportsTheServicesOwnVerdictAndNotAnEmptyList() throws Exception {
        try (var stub = new StubMatrix(request -> {
            assertEquals("/v1/assets/validate", request.path());
            return StubMatrix.Reply.json(200, """
                    {"valid":true,
                     "findings":[{"code":"mapping.authority-inert","path":"spec.authority",
                                  "message":"scope matches no property"}]}
                    """);
        }); var mx = stub.client()) {
            Validation validation = mx.validate("kind: ClaimMapping\n");
            // A handful of codes are advisory. Deriving the verdict from the
            // list length would call this asset invalid and disagree with the
            // service that enforces the rules.
            assertTrue(validation.valid());
            assertEquals(1, validation.findings().size());
            assertEquals("mapping.authority-inert", validation.findings().get(0).code());
        }
    }

    @Test
    void journalReadsTheEntriesKeyTheServiceSends() throws Exception {
        try (var stub = new StubMatrix(request -> StubMatrix.Reply.json(200, """
                {"entries":[{"kind":"apply","outcome":"ok","asset_ref":"crm@2"}],
                 "next_before":null}
                """)); var mx = stub.client()) {
            var entries = mx.journal(10);
            assertEquals(1, entries.size());
            assertEquals("apply", entries.get(0).path("kind").asText());
            assertEquals("limit=10", stub.seen().get(0).query());
        }
    }

    @Test
    void rollbackIsReportedBySupersessionCounts() throws Exception {
        try (var stub = new StubMatrix(request -> {
            assertTrue(request.body().contains("\"decision_id\":\"CHG-11\""));
            return StubMatrix.Reply.json(200, """
                    {"mapping":"captable@1","decision_id":"CHG-11","superseded":2,
                     "skipped_no_prior":1,"already_rolled_back":0,"disputed":0}
                    """);
        }); var mx = stub.client()) {
            RollbackOutcome out = mx.rollback("captable", "CHG-11");
            assertEquals(2L, out.superseded());
            // The honest case a rollback cannot fix: no prior value to restore.
            assertEquals(1L, out.skippedNoPrior());
        }
    }

    @Test
    void anAssetNameIsEncodedIntoTheRouteRatherThanReshapingIt() throws Exception {
        try (var stub = new StubMatrix(request -> StubMatrix.Reply.yaml(200, "kind: DataSource\n"));
                var mx = stub.client()) {
            assertEquals("kind: DataSource\n", mx.getYaml("datasources", "odd/name"));
            assertEquals("/v1/datasources/odd%2Fname", stub.paths().get(0));
        }
    }

    @Test
    void probeAnswersRatherThanThrowingWhenASourceIsDown() throws Exception {
        try (var stub = new StubMatrix(request -> StubMatrix.Reply.json(200, """
                {"source":"crm","reachable":false,"breaker":"open",
                 "detail":"connection refused"}
                """)); var mx = stub.client()) {
            Probe probe = mx.probe("crm");
            // "I asked, and it is down" is a successful probe.
            assertFalse(probe.reachable());
            assertEquals("open", probe.breaker());
            assertNull(probe.latencyMs());
        }
    }
}
