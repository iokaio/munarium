// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.model;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;

/** Multiturn sessions over a runbook's access-permitted collections. */
public final class SessionsApi {
    private SessionsApi() {}

    /**
     * API-level model override — honored only under the runbook's
     * {@code models.allowOverrides} policy; a disallowed override draws the
     * typed 403, never a silent downgrade.
     */
    public record ModelOverride(String provider, String model, String tier) {
        public static ModelOverride tier(String tier) {
            return new ModelOverride(null, null, tier);
        }

        public static ModelOverride provider(String provider) {
            return new ModelOverride(provider, null, null);
        }
    }

    public record CreateSessionResult(
            String sessionId, String runbookRef, List<String> permittedCollections) {}

    public record TurnHit(
            String collection,
            String chunkId,
            String sourceId,
            String sourcePath,
            String sourceContentHash,
            String text,
            double score) {}

    public record CollectionEnvelope(String collection, Retrieval.ProvenanceEnvelope envelope) {}

    /** Deterministic turn-verification outcome (empty violations = verified). */
    public record TurnVerification(
            List<String> checks,
            int retries,
            List<String> firstPassViolations,
            List<String> violations) {}

    public record TurnCompletion(
            String provider,
            String model,
            boolean wasOverride,
            String text,
            long inputTokens,
            long outputTokens,
            TurnVerification verification) {}

    /**
     * What one evidence layer produced. {@code role} is
     * {@code supporting | primary | controlling}, {@code requirement} is
     * {@code required | optional | fallback}, and {@code block} is
     * {@code document_hits | complete_table | count | fact_slice | refusal}
     * — modeled as strings, not enums, so a newer server's vocabulary
     * arrives readable instead of throwing.
     */
    public record LayerOutcome(
            String layer,
            String role,
            String requirement,
            String block,
            String evidenceId,
            boolean supportsCompleteness,
            String refusalCode,
            long elapsedMs) {}

    /**
     * Why the model saw what it saw — about the DECISION, not the
     * content: which profile ran, which layers answered or refused, and
     * whether a completeness claim was permissible at all. No evidence rows
     * appear here; resolve those through the evidence plane.
     */
    public record EvidenceHierarchyDecision(
            String profile,
            String intentKind,
            boolean intentExplicit,
            List<LayerOutcome> layers,
            boolean completenessAvailable,
            int disclosedConflicts,
            String conflictsPolicy) {}

    /**
     * {@code hierarchy} is present ONLY when a research profile ran. A legacy
     * turn leaves it null, and the shared mapper's NON_NULL inclusion keeps
     * the key out of the JSON entirely — the governing S-3.x invariant is
     * that a caller who does not use a profile sees byte-identical behaviour.
     */
    public record TurnResult(
            String sessionId,
            int ordinal,
            List<String> collectionsSearched,
            List<String> skipped,
            List<TurnHit> hits,
            List<CollectionEnvelope> envelopes,
            TurnCompletion completion,
            EvidenceHierarchyDecision hierarchy) {}

    /**
     * One progress event on the streaming turn plane. Modeled permissively —
     * {@link #stage} names the stage ({@code retrieval}/{@code merge}/
     * {@code model}/{@code completion}/{@code verify}, plus this
     * hierarchy stages {@code profile}/{@code layer_start}/
     * {@code layer_source}/{@code layer_complete}/{@code coverage}/
     * {@code compose}) and the other members are populated per stage; a
     * NEWER server may add stages this build cannot name, and they must flow
     * through rather than break the stream (progress is informational).
     *
     * <p>Every member is a BOXED type on purpose: an absent field must read
     * as null, never as a confident {@code 0}/{@code false} the server never
     * sent — a {@code coverage} event that failed to carry
     * {@code completeness_available} must not look like a denial.
     *
     * <p>The hierarchy stages are appended, not interleaved: the existing
     * stages' members keep their positions so a caller reading
     * {@code progress.hits()} is unaffected.
     */
    public record TurnProgress(
            String stage,
            String collection,
            Integer hits,
            Boolean skipped,
            String provider,
            String model,
            String tier,
            Boolean wasOverride,
            Integer attempt,
            Long inputTokens,
            Long outputTokens,
            List<String> checks,
            Integer violations,
            // verify (optional) + layer_start/layer_source/layer_complete
            String layer,
            // layer_start
            String role,
            String requirement,
            // layer_source; it also reuses `provider` above, which there names
            // the EVIDENCE provider (documents | facts | matrix), not a model vendor
            String source,
            // layer_complete
            String block,
            Boolean supportsCompleteness,
            String refusalCode,
            Long elapsedMs,
            // profile
            String profile,
            List<String> layers,
            String intentKind,
            Boolean intentExplicit,
            // coverage
            Boolean completenessAvailable,
            Integer disclosedConflicts,
            // compose
            Integer layersUsed,
            Integer contextChars,
            List<String> layersDropped,
            // selection / expansion (server-side since 2026-08-25, added here
            // 2026-08-29). APPENDED rather than grouped with the other legacy
            // stages above, because a record's components are positional and
            // inserting one silently renumbers every caller's constructor
            // call. Correct grouping is not worth a source break.
            //
            // Until now these events decoded with only `stage` set and
            // everything worth emitting dropped — and a caller reading
            // `probed == null` could not tell "the server did not send it"
            // from "this client cannot see it".
            //
            // selection: permitted collections probed with the original query,
            // and how many won the deep expanded search. The unselected ones
            // still contribute their probe pools to the merge.
            Integer probed,
            Integer selected,
            List<String> collections,
            // expansion: the accepted lexical variants. Possibly EMPTY, in
            // which case the original query searched alone — so an empty list
            // and a null are different readings. The paid call itself rides on
            // `provider`/`model`/`inputTokens`/`outputTokens` above.
            List<String> terms) {}

    /** One stored transcript row (JSON-shaped fields ride verbatim). */
    public record SessionTurn(
            int ordinal,
            String query,
            List<String> collectionsSearched,
            JsonNode hits,
            JsonNode envelope,
            JsonNode completion,
            String createdAt) {}

    public record Session(
            String sessionId,
            String uid,
            String runbookRef,
            int accessLevel,
            List<String> compartments,
            String state,
            String createdAt,
            List<SessionTurn> turns) {}
}
