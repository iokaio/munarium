// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.model;

import com.fasterxml.jackson.annotation.JsonProperty;
import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;
import java.util.Map;

/** Guided runbook authoring: patterns, drafts, validation, bundles. */
public final class Authoring {
    private Authoring() {}

    public record PatternSummary(
            String id,
            String name,
            String description,
            String startFrom,
            String guidance,
            boolean hasCompletion) {}

    public record PatternPage(List<PatternSummary> patterns) {}

    public record NamedYaml(String name, String yaml) {}

    public record PatternDetail(
            String id,
            String name,
            String description,
            String startFrom,
            String guidance,
            boolean hasCompletion,
            List<String> decisionNotes,
            String runbookYaml,
            List<NamedYaml> shapes) {}

    public record CreateDraftRequest(String name, String patternId, boolean seedFromExemplar) {
        public static CreateDraftRequest of(String name, String patternId) {
            return new CreateDraftRequest(name, patternId, false);
        }
    }

    public record InterviewQuestion(
            String id,
            String prompt,
            String guidance,
            String kind,
            boolean required,
            @JsonProperty("default") JsonNode defaultValue,
            List<String> choices,
            String mapsTo) {}

    public record InterviewSection(
            String id, String title, String docRef, List<InterviewQuestion> questions) {}

    public record DraftDocument(String path, String kind, String yaml, String sha256) {}

    public record DocumentFindings(String path, List<Runbooks.ValidationFinding> findings) {}

    public record DraftValidation(
            boolean valid,
            List<DocumentFindings> documents,
            List<Runbooks.ValidationFinding> setFindings,
            List<String> todos) {}

    public record DraftSummary(
            String draftId,
            String name,
            String state,
            String patternId,
            String createdBy,
            String updatedAt) {}

    public record DraftPage(List<DraftSummary> drafts) {}

    public record Draft(
            String draftId,
            String name,
            String state,
            String patternId,
            JsonNode answers,
            List<InterviewSection> interview,
            List<DraftDocument> documents,
            DraftValidation validation,
            List<String> todos,
            String assistNote,
            String createdBy,
            String createdAt,
            String updatedAt) {}

    public record DraftDelete(String draftId, String status) {}

    public record AssistRequest(String description, String instructions, String provider,
            String model, String tier) {
        public static AssistRequest empty() {
            return new AssistRequest(null, null, null, null, null);
        }
    }

    /** Assist NEVER fails — a degraded pass sets {@code assistNote}. */
    public record AssistResult(
            List<DraftDocument> documents,
            List<Runbooks.Suggestion> suggestions,
            String assistNote,
            DraftValidation validation) {}

    public record BundleTool(String name, String version) {}

    public record BundleValidation(boolean valid, long errors, long warns, long infos) {}

    /** The export bundle — self-contained and hash-manifested. */
    public record ExportBundle(
            String kind,
            @JsonProperty("apiVersion") String apiVersion,
            BundleTool tool,
            String draftId,
            String name,
            String createdAt,
            Map<String, String> files,
            Map<String, String> hashes,
            List<String> applyOrder,
            String manifestHash,
            BundleValidation validation) {}

    public record AppliedDoc(
            String path, String kind, @JsonProperty("ref") String ref, String yamlHash) {}

    public record ApplyDraftResult(List<AppliedDoc> applied) {}
}
