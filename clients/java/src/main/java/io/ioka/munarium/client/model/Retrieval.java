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

    public record SearchResult(List<SearchHit> hits, ProvenanceEnvelope envelope) {}

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
