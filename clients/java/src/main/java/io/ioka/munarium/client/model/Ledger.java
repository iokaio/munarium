// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.model;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;

/**
 * Wire models for the command/query planes — the append-only fact ledger.
 * Field names mirror the server's {@code munarium-api-types} (the JSON casing
 * truth); records deserialize permissively (unknown fields ignored).
 */
public final class Ledger {
    private Ledger() {}

    /** One deterministic-gate finding. */
    public record GateFinding(
            String ruleId, String severity, String message, String scopePath, JsonNode detail) {}

    /** Where a connector-originated claim came from. Null on every
     * model-extracted claim; provenance, never a gate input. */
    public record ClaimOrigin(
            String kind,
            String sourceId,
            String mappingVersion,
            String rowKey,
            String eventPosition,
            String observedAt,
            String evidenceId) {}

    /** One recorded claim. {@code status == "disputed"} is a SUCCESS state. */
    public record Claim(
            String id,
            String versionId,
            long seq,
            String claimType,
            String subject,
            String key,
            String value,
            String normalizedText,
            String scopePath,
            String status,
            String provenance,
            String supersedesId,
            String entityId,
            JsonNode evidence,
            Double confidence,
            String shapeRef,
            ClaimOrigin origin) {
        public boolean isDisputed() {
            return "disputed".equals(status);
        }
    }

    /**
     * One claim to propose (alone, or in an {@code appendEvents} batch).
     * Build via {@link #fact}/{@link #update}/{@link #correction} or the
     * canonical constructor for the long tail of fields.
     */
    public record ClaimInput(
            String subject,
            String key,
            String value,
            String claimType,
            String scopePath,
            String provenance,
            String supersedesId,
            String entityId,
            JsonNode evidence,
            Double confidence,
            String shapeRef,
            ClaimOrigin origin) {

        public static ClaimInput fact(String subject, String key, String value) {
            return new ClaimInput(
                    subject, key, value, "fact", null, null, null, null, null, null, null, null);
        }

        public static ClaimInput update(String subject, String key, String value) {
            return new ClaimInput(
                    subject, key, value, "update", null, null, null, null, null, null, null, null);
        }

        /** Corrections name what they supersede EXPLICITLY (append-only). */
        public static ClaimInput correction(
                String subject, String key, String value, String supersedesId) {
            return new ClaimInput(
                    subject, key, value, "correction", null, null, supersedesId, null, null, null,
                    null, null);
        }

        public ClaimInput withScopePath(String scope) {
            return new ClaimInput(
                    subject, key, value, claimType, scope, provenance, supersedesId, entityId,
                    evidence, confidence, shapeRef, origin);
        }

        /** The connector provenance a Matrix mapping attaches. */
        public ClaimInput withOrigin(ClaimOrigin o) {
            return new ClaimInput(
                    subject, key, value, claimType, scopePath, provenance, supersedesId, entityId,
                    evidence, confidence, shapeRef, o);
        }
    }

    /**
     * Result of {@code proposeClaim}. A gate-blocked claim is SUCCESS with
     * {@code isDisputed()} plus findings — recorded, never dropped
     * (invariant #1).
     */
    public record ClaimOutcome(Claim claim, List<GateFinding> findings, long headSeq) {
        public boolean isDisputed() {
            return claim.isDisputed();
        }
    }

    /** Result of {@code appendEvents} — the batch gated as ONE unit. */
    public record EventsOutcome(List<Claim> claims, List<GateFinding> findings, long headSeq) {
        public boolean isDisputed() {
            return claims.stream().anyMatch(Claim::isDisputed);
        }
    }

    public record ClaimLookup(Claim claim, boolean superseded, String supersededBy) {}

    public record FactsPage(List<Claim> facts, long asOfSeq, long headSeq) {}

    /** One persisted gate finding + the head seq its write settled at. */
    public record StoredFinding(long seq, GateFinding finding) {}
}
