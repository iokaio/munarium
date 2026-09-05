// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.model;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;

/** Anchors, promises, counters, digests, and the composed context. */
public final class Memory {
    private Memory() {}

    public record Anchor(
            String id,
            String versionId,
            String detailKey,
            String lockedValue,
            String lockedAtScope,
            String status,
            long seq) {}

    public record Promise(
            String id,
            String versionId,
            String key,
            String kind,
            String description,
            String originScope,
            String dueScope,
            String status,
            long seq,
            Long fulfilledSeq) {}

    public record Counter(String key, long total, Long budget) {}

    public record Digest(
            String versionId,
            int tier,
            String scopePath,
            String content,
            String contentHash,
            long builtFromSeq) {}

    public record Section(String title, String body) {}

    public record ComposedContext(
            List<Section> sections,
            String text,
            long estimatedTokens,
            String contentHash,
            long asOfSeq) {}

    /** Evidence for a lock_anchor call, forwarded verbatim. */
    public record Evidence(JsonNode value) {}
}
