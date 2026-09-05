// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.model;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;

/** Shapes, runbooks (v1 runs + the v2 surface), and chronology rules. */
public final class Runbooks {
    private Runbooks() {}

    public record ApplyShapeResult(String shapeRef, String yamlHash, String eventId) {}

    public record RunbookRun(String runId, String state) {}

    public record RunbookStep(int ordinal, String name, String state, JsonNode detail) {}

    public record RunStatus(
            String runId, String runbookRef, String state, String versionId,
            List<RunbookStep> steps) {}

    /** One collection a runbook spans, with its access requirements. */
    public record RunbookCollection(
            String name,
            String collectionId,
            String shapeRef,
            int accessLevel,
            List<String> compartments,
            String activeIndex,
            long sourceCount) {}

    public record RunbookSummary(
            String runbookRef,
            String name,
            int version,
            String status,
            int minAccessLevel,
            List<RunbookCollection> collections,
            String createdAt) {}

    public record RunbookInfo(
            String runbookRef,
            String name,
            int version,
            String status,
            List<RunbookCollection> collections,
            List<String> versions,
            JsonNode models,
            JsonNode retrieval,
            boolean hasCompletion,
            String createdAt) {}

    public record ValidationFinding(String severity, String code, String message, String path) {}

    /** AI-assisted improvement suggestion (advisory only). */
    public record Suggestion(String title, String rationale, String patchHint) {}

    public record ValidateResult(
            boolean valid,
            List<ValidationFinding> findings,
            List<Suggestion> suggestions,
            String suggestNote) {}

    /** First pass of the double-pass soft removal. */
    public record RemovalRequest(String runbookRef, String removalId, String expiresAt) {}

    /** Removal is visibility-only — yaml, runs, and index data are retained. */
    public record RemovalConfirm(String runbookRef, String status) {}

    /** Applied chronology-rules asset (the sixth gate's arming surface). */
    public record ChronologyRulesResult(String name, long ruleCount) {}
}
