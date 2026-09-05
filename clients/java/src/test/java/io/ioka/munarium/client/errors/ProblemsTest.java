// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.errors;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import io.ioka.munarium.client.model.Json;
import java.time.Duration;
import java.util.Map;
import org.junit.jupiter.api.Test;

/** The one error-construction path, exercised from both transports' inputs. */
class ProblemsTest {
    private static JsonNode problem(String slug, int status, String detail, String extraJson) {
        try {
            String body = "{\"type\":\"https://munarium.ioka.io/problems/" + slug + "\",\"title\":\""
                    + slug + "\",\"status\":" + status + ",\"detail\":\"" + detail + "\""
                    + (extraJson.isEmpty() ? "" : "," + extraJson) + "}";
            return Json.MAPPER.readTree(body);
        } catch (Exception e) {
            throw new AssertionError(e);
        }
    }

    @Test
    void headConflictDecodesExtensions() {
        var e = Problems.fromProblemJson(
                409, problem("head-conflict", 409, "d", "\"expected\":3,\"actual\":7"), null);
        var hc = assertInstanceOf(HeadConflictException.class, e);
        assertEquals(3, hc.expected());
        assertEquals(7, hc.actual());
    }

    @Test
    void policyRejectionCarriesFindings() {
        var e = Problems.fromProblemJson(422, problem("policy-rejection", 422, "d",
                "\"gate_findings\":[{\"rule_id\":\"gate.ledger-conflict\","
                        + "\"severity\":\"block\",\"message\":\"boom\"}]"), null);
        var pr = assertInstanceOf(PolicyRejectionException.class, e);
        assertEquals(1, pr.findings().size());
        assertEquals("gate.ledger-conflict", pr.findings().get(0).ruleId());
        assertEquals(1, pr.findingsTotal());
        assertFalse(pr.findingsTruncated());
    }

    @Test
    void identityAndLifecycleSlugsMapByStatusClass() {
        record Case(String slug, Class<? extends MunariumException> type) {}
        var cases = new Case[] {
            new Case("uid-required", InvalidInputException.class),
            new Case("uid-mismatch", ForbiddenException.class),
            new Case("token-expired", UnauthenticatedException.class),
            new Case("token-revoked", UnauthenticatedException.class),
            new Case("scope-missing", ForbiddenException.class),
            new Case("override-not-allowed", ForbiddenException.class),
            new Case("removal-not-confirmed", InvalidInputException.class),
            new Case("runbook-removed", NotFoundException.class),
            new Case("session-not-open", InvalidInputException.class),
            new Case("authoring-draft-invalid", InvalidInputException.class),
        };
        for (var c : cases) {
            var e = Problems.fromProblemJson(400, problem(c.slug(), 400, "d", ""), null);
            assertInstanceOf(c.type(), e, c.slug());
        }
    }

    @Test
    void runLockedIsTypedAndNeverAutoRetried() {
        // Before this slug was mapped it decoded as unexpected — hiding that
        // the request was rejected pre-execution and a later re-run succeeds
        // once the lock clears.
        var e = Problems.fromProblemJson(
                409, problem("run-locked", 409, "run run-1 holds the lock", ""), null);
        var rl = assertInstanceOf(RunLockedException.class, e);
        assertEquals("run-locked", rl.slug());
        assertFalse(rl.isTransient(),
                "a run lock is held for a whole run — pace yourself, like rate-limited;"
                        + " sub-second auto-retry would be futile churn");
    }

    @Test
    void grpcErrorInfoRoundTrip() {
        var e = Problems.fromGrpcInfo("head-conflict", "head conflict",
                Map.of("expected", "3", "actual", "7"));
        var hc = assertInstanceOf(HeadConflictException.class, e);
        assertEquals(3, hc.expected());
        assertEquals(7, hc.actual());

        var truncated = Problems.fromGrpcInfo("policy-rejection", "d", Map.of(
                "gate_findings",
                "[{\"rule_id\":\"gate.ledger-conflict\",\"severity\":\"block\",\"message\":\"m\"}]",
                "findings_total", "40",
                "findings_truncated", "true"));
        var pr = assertInstanceOf(PolicyRejectionException.class, truncated);
        assertEquals(1, pr.findings().size());
        assertEquals(40, pr.findingsTotal());
        assertTrue(pr.findingsTruncated());
    }

    @Test
    void rateLimitedCarriesRetryAfterAndOverloadedIsTransient() {
        var rl = Problems.fromProblemJson(
                429, problem("rate-limited", 429, "tpm cap", ""), Duration.ofSeconds(7));
        assertEquals(Duration.ofSeconds(7),
                assertInstanceOf(RateLimitedException.class, rl).retryAfter().orElseThrow());
        assertFalse(rl.isTransient(), "honor retry_after in your own pacing");

        var ov = Problems.fromProblemJson(503, problem("overloaded", 503, "drain", ""), null);
        assertInstanceOf(OverloadedException.class, ov);
        assertTrue(ov.isTransient());
    }

    @Test
    void unknownSlugOnGatewayStatusIsTransientUnexpected() {
        var e = Problems.fromProblemJson(503, problem("mystery", 503, "gw", ""), null);
        assertInstanceOf(UnexpectedServerException.class, e);
        assertTrue(e.isTransient(), "5xx gateway statuses stay read-retryable");
        var e400 = Problems.fromProblemJson(400, problem("mystery", 400, "no", ""), null);
        assertFalse(e400.isTransient());
    }

    @Test
    void notFoundUsesKindAndId() {
        var e = Problems.fromProblemJson(404,
                problem("not-found", 404, "not found: claim c-1", "\"kind\":\"claim\",\"id\":\"c-1\""),
                null);
        var nf = assertInstanceOf(NotFoundException.class, e);
        assertEquals("claim", nf.kind());
        assertEquals("c-1", nf.id());
    }
}
