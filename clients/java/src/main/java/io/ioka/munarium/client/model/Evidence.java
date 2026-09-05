// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.model;

import com.fasterxml.jackson.annotation.JsonProperty;
import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;

/**
 * The sealed evidence plane's read models.
 *
 * <p>The MANIFEST is deliberately not mirrored as a Java record. It is defined
 * by the cross-tree contract ({@code contract/matrix/evidence-manifest.schema.json})
 * and returned verbatim, so a hand-written mirror here would be a second
 * definition of a schema this client does not own — and the first thing to
 * drift when the contract adds an optional field. It is a {@link JsonNode}.
 *
 * <p>Rows are different: their envelope is this server's, not the contract's,
 * so it is typed.
 */
public final class Evidence {
    private Evidence() {}

    /**
     * A bounded window over a sealed artifact's rows.
     *
     * <p>Served for canonical-CSV artifacts only. A Parquet artifact is sealed
     * and replayable byte-for-byte, but the server does not decode it and says
     * so rather than pretending the rows are unavailable.
     *
     * @param evidenceId the artifact
     * @param from zero-based index of the first row returned
     * @param rows the page; each row is an object keyed by the manifest's
     *     column NAMES, not by the artifact's own header
     * @param total total rows in the artifact, when the serialization allows
     *     counting them without decoding everything
     * @param hasMore whether more rows follow this page
     */
    public record EvidenceRows(
            @JsonProperty("evidence_id") String evidenceId,
            @JsonProperty("from") int from,
            List<JsonNode> rows,
            Integer total,
            @JsonProperty("has_more") boolean hasMore) {}
}
