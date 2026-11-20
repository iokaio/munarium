// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.model;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;

/** Hybrid search, index status, and compartmentalized collections. */
public final class Retrieval {
    private Retrieval() {}

    public record SearchHit(
            String chunkId,
            String sourceId,
            String sourcePath,
            String sourceContentHash,
            String text,
            double score,
            Integer lexicalRank,
            Integer vectorRank,
            JsonNode metadata) {}

    /**
     * Every retrieval answer carries one — surface it, don't hide it
     * (invariant #4). Sources are named three ways deliberately: ids are
     * stable identity, paths say WHICH document answered, and content
     * hashes prove which bytes it held.
     */
    public record ProvenanceEnvelope(
            List<String> chunkIds,
            List<String> sourceIds,
            List<String> sourcePaths,
            List<String> sourceContentHashes,
            String indexVersion,
            long eventWatermark,
            String providerFingerprint) {}

    /**
     * {@code execution} is present only when the request set
     * {@code discloseExecution}, or when the collection is enrolled for
     * inference (ClearView R1/IR-0). A v1.4 server never sends it, and a
     * null re-serializes without the key.
     */
    public record SearchResult(
            List<SearchHit> hits, ProvenanceEnvelope envelope, SearchExecution execution) {
        /** The 1.4 component list: no execution disclosure. */
        public SearchResult(List<SearchHit> hits, ProvenanceEnvelope envelope) {
            this(hits, envelope, null);
        }
    }

    /**
     * How one search actually executed (ClearView R1). The envelope says what
     * the index was; this says how the query ran. {@code engine} is
     * {@code postgresql | datastore}; scores from different
     * {@code lexicalDomain}s are not comparable. {@code stages} is REST-only
     * (the typed gRPC message has no stages) and null when absent.
     */
    public record SearchExecution(
            int version,
            String engine,
            String lexicalDomain,
            List<SearchLeg> legs,
            List<SearchStage> stages) {}

    /**
     * One retrieval leg ({@code lexical | vector}). A null member was not
     * measured by the serving engine: unknown, never zero or {@code false}.
     */
    public record SearchLeg(String leg, Long candidateLimit, Long fetched, Boolean exhausted) {}

    /**
     * One inference stage's receipt on an enrolled read (ClearView IR-0/I1).
     * {@code stage} is {@code embedding | reranking}; {@code policy} is
     * {@code require | prefer}; {@code state} is {@code ready} when it ran.
     * Content-free: no query, passage or endpoint detail.
     */
    public record SearchStage(
            String stage,
            String policy,
            long policyRevision,
            boolean applied,
            String state,
            String reason,
            String fallback,
            String identity,
            Long scored) {}

    public record IndexStatus(
            String indexVersion,
            String shapeRef,
            long eventWatermark,
            boolean active,
            JsonNode manifest) {}

    /** A compartmentalized collection. No delete exists — retirement is soft. */
    public record CollectionInfo(
            String id,
            String name,
            String shapeRef,
            int accessLevel,
            List<String> compartments,
            String status,
            String description,
            String createdAt,
            long sourceCount,
            String activeIndex) {}
}
