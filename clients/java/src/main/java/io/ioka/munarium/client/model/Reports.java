// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.model;

import com.fasterxml.jackson.annotation.JsonProperty;
import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;

/** Management reports over the interactions audit trail (mgmt role). */
public final class Reports {
    private Reports() {}

    public record UsageRow(
            String key,
            long interactions,
            long turns,
            long completionInputTokens,
            long completionOutputTokens,
            Double avgLatencyMs) {}

    public record UsageReport(String groupBy, String from, String to, List<UsageRow> rows) {}

    public record AuditEntry(
            String id,
            String uid,
            String sessionId,
            String requestId,
            String plane,
            String method,
            String runbookRef,
            String tokenJti,
            Integer status,
            Integer latencyMs,
            JsonNode request,
            JsonNode response,
            String createdAt) {}

    /** {@code nextBefore} is the keyset cursor — absent = trail exhausted. */
    public record AuditPage(List<AuditEntry> entries, String nextBefore) {}

    public record CostRow(
            String provider,
            String model,
            long turns,
            long overriddenTurns,
            long inputTokens,
            long outputTokens) {}

    public record CostReport(String from, String to, List<CostRow> rows) {}

    public record TimeseriesBucket(
            String bucket,
            long requests,
            @JsonProperty("errors_4xx") long errors4xx,
            @JsonProperty("errors_5xx") long errors5xx,
            @JsonProperty("p50_latency_ms") Double p50LatencyMs,
            @JsonProperty("p95_latency_ms") Double p95LatencyMs) {}

    public record TimeseriesReport(
            String window, long bucketSeconds, String plane, List<TimeseriesBucket> buckets) {}

    public record EndpointRow(
            String method,
            long requests,
            double errorRate,
            Double avgLatencyMs,
            @JsonProperty("p95_latency_ms") Double p95LatencyMs) {}

    public record EndpointsReport(String window, List<EndpointRow> rows) {}

    public record RunbookRunsRow(String state, long runs, Double avgWallMs) {}

    public record RunbookStepsRow(String state, long steps) {}

    public record RunbookReport(
            String window, List<RunbookRunsRow> runs, List<RunbookStepsRow> steps) {}

    public record SessionsBucket(String bucket, long sessionsOpened, long turns, long activeUids) {}

    public record SessionsReport(String window, long bucketSeconds, List<SessionsBucket> buckets) {}

    /**
     * One evidence layer's aggregate behaviour over the window.
     * {@code refusalCodes} is most-frequent-first. The latency members are
     * pinned with {@code @JsonProperty} rather than left to the naming
     * strategy, matching this file's rule for digit-adjacent names.
     */
    public record EvidenceLayerStats(
            String profile,
            String layer,
            long turns,
            long refusals,
            long complete,
            List<String> refusalCodes,
            @JsonProperty("p50_ms") long p50Ms,
            @JsonProperty("p95_ms") long p95Ms) {}

    /**
     * How the evidence hierarchy actually behaved. The operational
     * question it answers is "which layer is quietly refusing?" — a layer
     * refusing on most turns still returns 200 to every caller, so the
     * answers are thinner than the runbook claims while nothing goes red.
     */
    public record EvidenceReport(
            String window,
            long hierarchyTurns,
            long legacyTurns,
            long completenessAvailable,
            List<EvidenceLayerStats> layers) {}

    /** One Matrix data view declared across the tenant's applied runbooks. */
    public record MatrixDataView(
            String runbookRef, String name, String contract, int accessLevel) {}

    /**
     * Munarium Matrix's health as the SERVER sees it.
     * {@code configured} false means the plane is not wired at all, which is
     * a different fact from wired-and-failing and must not read the same.
     * {@code circuitOpen} is per server instance, never per tenant.
     */
    public record MatrixReport(
            boolean configured,
            boolean circuitOpen,
            long consecutiveFailures,
            List<MatrixDataView> dataViews) {}
}
