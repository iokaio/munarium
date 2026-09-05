// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.planes;

import com.fasterxml.jackson.databind.JsonNode;
import io.ioka.munarium.client.model.SessionsApi.ModelOverride;
import java.io.ByteArrayInputStream;
import java.io.InputStream;
import java.util.List;

/**
 * Option records for plane methods with optional/filter arguments. Every
 * record has a minimal static factory; use the canonical constructor for the
 * long tail. {@code null} always means "not sent".
 */
public final class Params {
    private Params() {}

    /** Query options for {@code query.facts}. */
    public record FactsQuery(
            String scopePrefix, Long asOfSeq, List<String> statuses, Integer limit) {
        public static FactsQuery all() {
            return new FactsQuery(null, null, List.of(), null);
        }

        public static FactsQuery atSeq(long asOfSeq) {
            return new FactsQuery(null, asOfSeq, List.of(), null);
        }
    }

    /** Query options for {@code query.composeContext}. */
    public record ContextQuery(String scope, Long budgetTokens, Integer factLimit, Long asOfSeq) {
        public static ContextQuery defaults() {
            return new ContextQuery(null, null, null, null);
        }
    }

    /** Query options for {@code query.findings} (info | warn | block). */
    public record FindingsQuery(Long asOfSeq, String severity, String ruleId, Integer limit) {
        public static FindingsQuery all() {
            return new FindingsQuery(null, null, null, null);
        }

        public static FindingsQuery severity(String severity) {
            return new FindingsQuery(null, severity, null, null);
        }
    }

    /** Hybrid search request. */
    public record SearchQuery(
            String query, String shapeRef, Integer topK, String indexVersion, JsonNode filter) {
        public static SearchQuery of(String query, String shapeRef) {
            return new SearchQuery(query, shapeRef, null, null, null);
        }

        public SearchQuery withTopK(int k) {
            return new SearchQuery(query, shapeRef, k, indexVersion, filter);
        }
    }

    /** Create-or-update spec for a compartmentalized collection. */
    public record CollectionSpec(
            String name,
            String shapeRef,
            int accessLevel,
            List<String> compartments,
            String description) {
        public static CollectionSpec of(String name, String shapeRef) {
            return new CollectionSpec(name, shapeRef, 0, List.of(), null);
        }
    }

    /** Options for {@code runbooks.validate} — suggest spends provider tokens. */
    public record ValidateOptions(boolean suggest, String provider, String model, String tier) {
        public static ValidateOptions deterministic() {
            return new ValidateOptions(false, null, null, null);
        }
    }

    /** Filters for {@code tokens.list} (active = unexpired + unrevoked). */
    public record TokenListQuery(String uid, Boolean active) {
        public static TokenListQuery all() {
            return new TokenListQuery(null, null);
        }

        public static TokenListQuery forUid(String uid) {
            return new TokenListQuery(uid, null);
        }
    }

    /** Filters for {@code reports.usage} (groupBy: uid|session|runbook|collection). */
    public record UsageQuery(String groupBy, String from, String to) {
        public static UsageQuery byUid() {
            return new UsageQuery("uid", null, null);
        }
    }

    /** Filters for {@code reports.audit}; {@code before} is the keyset cursor. */
    public record AuditQuery(
            String uid,
            String sessionId,
            String runbook,
            String from,
            String to,
            Long limit,
            boolean bodies,
            String before) {
        public static AuditQuery forUid(String uid) {
            return new AuditQuery(uid, null, null, null, null, null, false, null);
        }
    }

    /** Metadata for a content-addressed source upload. */
    public record SourceMeta(
            String declaredSha256, String mediaType, String filename, String shapeRef) {
        /** {@code filename} is REQUIRED by the server — identity + matcher key. */
        public static SourceMeta of(String filename, String mediaType) {
            return new SourceMeta(null, mediaType, filename, null);
        }

        public SourceMeta withSha256(String hex) {
            return new SourceMeta(hex, mediaType, filename, shapeRef);
        }

        public SourceMeta withShapeRef(String ref) {
            return new SourceMeta(declaredSha256, mediaType, filename, ref);
        }
    }

    /**
     * A REPLAYABLE byte source for {@code ingest.putSource}: {@link #open()}
     * is called once per upload attempt, so the transport can retry a
     * transient failure with a fresh stream — safe because uploads are
     * idempotent by content address. The contract is the factory's: an
     * implementation that can serve only ONE fresh stream would make a
     * retried attempt upload short bytes.
     */
    @FunctionalInterface
    public interface ChunkSource {
        InputStream open();

        static ChunkSource ofBytes(byte[] bytes) {
            byte[] copy = bytes.clone();
            return () -> new ByteArrayInputStream(copy);
        }
    }

    /** One promise to open. */
    public record PromiseInput(
            String key, String kind, String description, String originScope, String dueScope) {
        public static PromiseInput of(String key, String kind, String description) {
            return new PromiseInput(key, kind, description, null, null);
        }
    }

    /** One anchor to lock. */
    public record AnchorInput(
            String subject, String key, String value, String scopePath, JsonNode evidence) {
        public static AnchorInput of(String subject, String key, String value) {
            return new AnchorInput(subject, key, value, null, null);
        }
    }

    /**
     * One session turn. {@code researchProfile} runs the turn
     * through a named evidence hierarchy; null is the legacy single-layer
     * document path and is NOT sent, so an existing caller's request bytes
     * are unchanged.
     */
    public record TurnOptions(
            String query,
            Integer topK,
            Boolean complete,
            ModelOverride modelOverride,
            String researchProfile) {
        public static TurnOptions of(String query) {
            return new TurnOptions(query, null, null, null, null);
        }

        /** Run the runbook's completion step under an optional override. */
        public TurnOptions withCompletion(ModelOverride override) {
            return new TurnOptions(query, topK, true, override, researchProfile);
        }

        /** Route this turn through a named research profile. */
        public TurnOptions withResearchProfile(String profile) {
            return new TurnOptions(query, topK, complete, modelOverride, profile);
        }
    }

    /** Provider completion request ({@code default} engages the fallback chain). */
    public record CompleteOptions(
            String prompt,
            String system,
            String model,
            String provider,
            String tier,
            Integer maxTokens,
            Double temperature,
            String versionId) {
        public static CompleteOptions of(String prompt) {
            return new CompleteOptions(prompt, null, null, null, null, null, null, null);
        }
    }

    /** Provider embedding request. */
    public record EmbedOptions(
            List<String> inputs, String model, String provider, String versionId) {
        public static EmbedOptions of(List<String> inputs) {
            return new EmbedOptions(List.copyOf(inputs), null, null, null);
        }
    }

    /**
     * Query options for {@code evidenceRows}. Both null = the server's
     * defaults (from 0, 100 rows, capped server-side at 1000).
     */
    public record EvidenceRowsQuery(Integer from, Integer limit) {
        public static EvidenceRowsQuery all() {
            return new EvidenceRowsQuery(null, null);
        }
    }
}
