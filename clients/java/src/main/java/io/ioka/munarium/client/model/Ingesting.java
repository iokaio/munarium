// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.model;

import java.util.Base64;
import java.util.List;

/**
 * Wire models for the ingest plane: streamed content-addressed sources, the
 * file/batch plane, and bulk upload sessions.
 */
public final class Ingesting {
    private Ingesting() {}

    public record PutSourceResult(
            String sourceId, String contentHash, long bytesLen, boolean alreadyExisted) {}

    public record RecordIngestResult(String eventId, long seq) {}

    /**
     * One file for the ingest plane. Content is base64 (JSON-safe on
     * REST; the gRPC transport decodes it back to raw bytes for the wire).
     * {@link #of} does the encoding for you.
     */
    public record IngestFile(
            String filename,
            String mediaType,
            String contentBase64,
            String sha256,
            List<String> collections) {

        public static IngestFile of(String filename, String mediaType, byte[] content) {
            return new IngestFile(
                    filename, mediaType, Base64.getEncoder().encodeToString(content), null, null);
        }

        public static IngestFile ofText(String filename, String mediaType, String text) {
            return of(filename, mediaType, text.getBytes(java.nio.charset.StandardCharsets.UTF_8));
        }

        /** Explicit collection targets (absent = matcher auto-binding). */
        public IngestFile withCollections(List<String> names) {
            return new IngestFile(filename, mediaType, contentBase64, sha256, List.copyOf(names));
        }
    }

    /** Per-file outcome — one failed file never fails the batch. */
    public record IngestResult(
            String filename,
            String sourceId,
            String sha256,
            boolean existed,
            List<String> boundTo,
            String error) {}

    public record IngestBatchResult(List<IngestResult> results) {}

    /** One bulk-manifest entry: what the client intends to upload. */
    public record BulkManifestEntry(
            String filename, String sha256, long bytesLen, String mediaType) {}

    public record BulkOpenResult(
            String bulkId, long total, long alreadyPresent, List<String> needed) {}

    public record BulkChunkResult(
            String bulkId,
            List<IngestResult> results,
            long stored,
            long skippedExisting,
            long pending,
            long failed) {}

    public record BulkFileError(String filename, String error) {}

    public record BulkStatus(
            String bulkId,
            String label,
            String status,
            long total,
            long stored,
            long skippedExisting,
            long pending,
            long failed,
            List<BulkFileError> failures,
            List<String> needed,
            String createdAt,
            String expiresAt,
            String completedAt) {}

    public record BulkCompleteResult(
            String bulkId,
            String status,
            long total,
            long stored,
            long skippedExisting,
            List<String> missing,
            long missingCount,
            List<String> mismatched,
            long mismatchedCount) {}

    /** Where a document actually went — metadata only, never the bytes. */
    public record SourceInfo(
            String sourceId,
            String filename,
            String mediaType,
            String contentHash,
            long bytesLen,
            String storageBackend,
            String blobUri,
            String extractionStatus,
            String extractionMethod,
            String createdAt) {}
}
