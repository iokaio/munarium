// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.model;

import java.util.List;

/** The BYOK provider gateway. */
public final class Providers {
    private Providers() {}

    public record ProviderHealth(
            boolean healthy, String provider, String endpointFingerprint, String detail) {}

    public record CompleteResult(
            String text,
            String stopReason,
            long inputTokens,
            long outputTokens,
            String provider,
            String model,
            String invocationEventId) {}

    public record EmbedResult(
            List<List<Double>> vectors,
            long dimensions,
            boolean cacheHit,
            String provider,
            String model,
            String invocationEventId) {}

    /** One /healthai probe — a small LIVE completion (spends real tokens). */
    public record HealthAiCheck(
            String provider,
            String tier,
            String model,
            boolean ok,
            boolean skipped,
            Long latencyMs,
            String detail) {}

    public record HealthAiResult(boolean healthy, List<HealthAiCheck> checks) {}

    /**
     * One provider config's resolved tier models — free introspection
     * ({@code GET /v1/providers}), zero provider calls, credential never
     * echoed.
     */
    public record ProviderModels(
            String name,
            String provider,
            String source,
            boolean credentialOk,
            String fast,
            String capable,
            String frontier) {}

    public record ProviderList(List<ProviderModels> providers) {}

    /**
     * The per-call output-token ceilings ({@code max_tokens}) the server
     * hands a model provider, one per kind of paid call, as ONE object
     * ({@code POST /v1/max-tokens}). Every member is REQUIRED on the wire:
     * the route replaces the tenant's whole set, never part of it, so the
     * record carries all eight as primitives and a body can never omit one.
     * Each is an unsigned 32-bit integer server-side (hence {@code long}),
     * range-checked on replace: {@code turnCompletion} 256..=16384,
     * {@code queryExpansion} 32..=512, the rest 1..=65536 — a miss is
     * {@code invalid-input}, not a clamp.
     *
     * <p>Precedence at call time: a runbook's own declaration where the
     * grammar has one ({@code completion.maxTokens},
     * {@code modelQueryExpansion.maxTokens}) &gt; this set &gt; the process's
     * {@code MUNARIUM_MAX_TOKENS_*} environment &gt; the built-ins.
     *
     * <p>The {@code with*} members exist because there is no partial update:
     * read {@link MaxTokensResponse#budgets()}, change one, send the whole
     * set back.
     */
    public record MaxTokensBudgets(
            long turnCompletion,
            long queryExpansion,
            long completeDefault,
            long healthaiProbe,
            long hierarchyClassifier,
            long hierarchyIntent,
            long runbookAdvisory,
            long authoringAssist) {

        /** A session turn's answer (a runbook's {@code completion.maxTokens} overrides it). */
        public MaxTokensBudgets withTurnCompletion(long v) {
            return new MaxTokensBudgets(v, queryExpansion, completeDefault, healthaiProbe,
                    hierarchyClassifier, hierarchyIntent, runbookAdvisory, authoringAssist);
        }

        /** The {@code modelQueryExpansion} variant-generation call. */
        public MaxTokensBudgets withQueryExpansion(long v) {
            return new MaxTokensBudgets(turnCompletion, v, completeDefault, healthaiProbe,
                    hierarchyClassifier, hierarchyIntent, runbookAdvisory, authoringAssist);
        }

        /** {@code POST /v1/providers/{name}/complete} when the request omits {@code max_tokens}. */
        public MaxTokensBudgets withCompleteDefault(long v) {
            return new MaxTokensBudgets(turnCompletion, queryExpansion, v, healthaiProbe,
                    hierarchyClassifier, hierarchyIntent, runbookAdvisory, authoringAssist);
        }

        /** Each {@code /healthai} probe completion. */
        public MaxTokensBudgets withHealthaiProbe(long v) {
            return new MaxTokensBudgets(turnCompletion, queryExpansion, completeDefault, v,
                    hierarchyClassifier, hierarchyIntent, runbookAdvisory, authoringAssist);
        }

        /** The evidence hierarchy's one-word question classifier. */
        public MaxTokensBudgets withHierarchyClassifier(long v) {
            return new MaxTokensBudgets(turnCompletion, queryExpansion, completeDefault,
                    healthaiProbe, v, hierarchyIntent, runbookAdvisory, authoringAssist);
        }

        /** The evidence hierarchy's semantic-intent task (names only). */
        public MaxTokensBudgets withHierarchyIntent(long v) {
            return new MaxTokensBudgets(turnCompletion, queryExpansion, completeDefault,
                    healthaiProbe, hierarchyClassifier, v, runbookAdvisory, authoringAssist);
        }

        /** The runbook validation AI advisory pass. */
        public MaxTokensBudgets withRunbookAdvisory(long v) {
            return new MaxTokensBudgets(turnCompletion, queryExpansion, completeDefault,
                    healthaiProbe, hierarchyClassifier, hierarchyIntent, v, authoringAssist);
        }

        /** The guided-authoring assist draft. */
        public MaxTokensBudgets withAuthoringAssist(long v) {
            return new MaxTokensBudgets(turnCompletion, queryExpansion, completeDefault,
                    healthaiProbe, hierarchyClassifier, hierarchyIntent, runbookAdvisory, v);
        }
    }

    /**
     * {@code GET /v1/max-tokens}, and what {@code POST /v1/max-tokens}
     * answers with: the effective budgets FLATTENED at the top level (so a
     * GET body round-trips into a POST body) plus where they come from.
     * {@code source} is {@code tenant} after the tenant replaced the set
     * through the API, else {@code environment} (the process's env vars over
     * the built-ins); {@code updatedAt} is the RFC 3339 instant of the last
     * replacement and {@code null} — never an empty string — for
     * {@code environment}.
     */
    public record MaxTokensResponse(
            long turnCompletion,
            long queryExpansion,
            long completeDefault,
            long healthaiProbe,
            long hierarchyClassifier,
            long hierarchyIntent,
            long runbookAdvisory,
            long authoringAssist,
            String source,
            String updatedAt) {

        /**
         * The eight budgets as a replace body — the read-modify-write seam,
         * since the route has no partial update. (Not a bean getter, so it
         * never re-serializes as a member.)
         */
        public MaxTokensBudgets budgets() {
            return new MaxTokensBudgets(turnCompletion, queryExpansion, completeDefault,
                    healthaiProbe, hierarchyClassifier, hierarchyIntent, runbookAdvisory,
                    authoringAssist);
        }
    }
}
