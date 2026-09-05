// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.planes;

import com.fasterxml.jackson.databind.JsonNode;
import io.ioka.munarium.client.model.Authoring;
import io.ioka.munarium.client.model.Evidence;
import io.ioka.munarium.client.model.Ingesting;
import io.ioka.munarium.client.model.Ledger;
import io.ioka.munarium.client.model.Memory;
import io.ioka.munarium.client.model.Providers;
import io.ioka.munarium.client.model.Reports;
import io.ioka.munarium.client.model.Retrieval;
import io.ioka.munarium.client.model.Runbooks;
import io.ioka.munarium.client.model.SessionsApi;
import io.ioka.munarium.client.model.Tokens;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.function.Consumer;

/**
 * The {@code CompletableFuture} twins of {@link Planes} — identical
 * semantics, method for method. Futures fail with the same
 * {@link io.ioka.munarium.client.errors.MunariumException} subclasses (as the
 * future's exception, unwrapped from {@code CompletionException} by the
 * standard {@code exceptionally}/{@code join} handling).
 *
 * <p>Implementation note (deliberate, documented): the engine is sync-first
 * and the async surface offloads each call to a VIRTUAL thread — on Java 21
 * blocking is the scalable primitive, so this is one implementation with
 * zero sync/async drift rather than two that must be kept aligned (the same
 * trade the Python client made for its async gRPC).
 */
public final class AsyncPlanes {
    private AsyncPlanes() {}

    public interface CommandsPlane {
        CompletableFuture<String> createVersion(
                String parentVersionId, JsonNode metadata, String idempotencyKey);

        default CompletableFuture<String> createVersion() {
            return createVersion(null, null, null);
        }

        CompletableFuture<Ledger.ClaimOutcome> proposeClaim(
                String versionId, Ledger.ClaimInput claim, Long expectedHead, String idempotencyKey);

        CompletableFuture<Ledger.EventsOutcome> appendEvents(
                String versionId,
                List<Ledger.ClaimInput> claims,
                String candidateText,
                Long expectedHead,
                String idempotencyKey);

        CompletableFuture<Memory.Promise> openPromise(
                String versionId, Params.PromiseInput promise, String idempotencyKey);

        CompletableFuture<Boolean> fulfillPromise(
                String versionId, String key, String idempotencyKey);

        CompletableFuture<Memory.Anchor> lockAnchor(
                String versionId, Params.AnchorInput anchor, String idempotencyKey);

        CompletableFuture<Void> recordCounts(
                String versionId,
                String key,
                String scopePath,
                long count,
                Long budget,
                String idempotencyKey);

        CompletableFuture<Void> upsertDigest(Memory.Digest digest);
    }

    public interface QueryPlane {
        CompletableFuture<Long> head(String versionId);

        CompletableFuture<Ledger.ClaimLookup> getClaim(String claimId);

        CompletableFuture<Ledger.FactsPage> facts(String versionId, Params.FactsQuery query);

        CompletableFuture<List<String>> lineage(String versionId);

        CompletableFuture<List<Memory.Anchor>> anchors(String versionId, Long asOfSeq);

        CompletableFuture<List<Memory.Promise>> promises(
                String versionId, Long asOfSeq, String status);

        CompletableFuture<List<Memory.Counter>> counters(String versionId, Long asOfSeq);

        CompletableFuture<List<Memory.Digest>> digests(String versionId);

        CompletableFuture<List<Ledger.StoredFinding>> findings(
                String versionId, Params.FindingsQuery query);

        CompletableFuture<Memory.ComposedContext> composeContext(
                String versionId, Params.ContextQuery query);
    }

    public interface IngestPlane {
        CompletableFuture<Ingesting.PutSourceResult> putSource(
                Params.ChunkSource data, Params.SourceMeta meta);

        CompletableFuture<Ingesting.RecordIngestResult> recordIngest(
                String versionId, String contentHash, String shapeRef);

        CompletableFuture<Ingesting.IngestResult> ingest(Ingesting.IngestFile file);

        CompletableFuture<List<Ingesting.IngestResult>> ingestBatch(
                List<Ingesting.IngestFile> files);

        CompletableFuture<Ingesting.BulkOpenResult> bulkOpen(
                List<Ingesting.BulkManifestEntry> files, String label);

        CompletableFuture<Ingesting.BulkChunkResult> bulkChunk(
                String bulkId, List<Ingesting.IngestFile> files);

        CompletableFuture<Ingesting.BulkStatus> bulkStatus(String bulkId, boolean includeNeeded);

        CompletableFuture<Ingesting.BulkCompleteResult> bulkComplete(String bulkId);

        CompletableFuture<Ingesting.SourceInfo> getSource(String sourceId);
    }

    public interface RetrievalPlane {
        CompletableFuture<Retrieval.SearchResult> search(Params.SearchQuery query);

        CompletableFuture<Retrieval.IndexStatus> indexStatus(String shapeRef);

        CompletableFuture<Retrieval.IndexStatus> buildIndex(String shapeRef, String versionId);

        CompletableFuture<Retrieval.CollectionInfo> createCollection(Params.CollectionSpec spec);

        CompletableFuture<List<Retrieval.CollectionInfo>> listCollections();

        CompletableFuture<Retrieval.CollectionInfo> getCollection(String id);
    }

    public interface RunbooksPlane {
        CompletableFuture<Runbooks.ApplyShapeResult> applyShape(String yaml, String versionId);

        CompletableFuture<String> applyRunbook(String yaml);

        CompletableFuture<Runbooks.RunbookRun> runRunbook(String name, String versionId);

        CompletableFuture<Runbooks.RunStatus> getRun(String runId);

        CompletableFuture<Runbooks.RunbookRun> approveStep(String runId, int ordinal);

        CompletableFuture<List<Runbooks.RunbookSummary>> list(boolean includeRemoved);

        CompletableFuture<Runbooks.RunbookInfo> getInfo(String name);

        CompletableFuture<Runbooks.ValidateResult> validate(
                String yaml, Params.ValidateOptions options);

        CompletableFuture<Runbooks.RemovalRequest> removeRequest(String name);

        CompletableFuture<Runbooks.RemovalConfirm> removeConfirm(String name, String removalId);

        CompletableFuture<Runbooks.ChronologyRulesResult> applyChronologyRules(String yaml);

        CompletableFuture<String> getChronologyRules(String name);
    }

    public interface ProvidersPlane {
        CompletableFuture<String> applyConfig(String yaml);

        CompletableFuture<Providers.ProviderHealth> health(String name);

        CompletableFuture<Providers.HealthAiResult> healthAi();

        CompletableFuture<Providers.CompleteResult> complete(
                String name, Params.CompleteOptions options);

        CompletableFuture<Providers.EmbedResult> embed(String name, Params.EmbedOptions options);

        CompletableFuture<Providers.ProviderList> list();

        CompletableFuture<Providers.MaxTokensResponse> maxTokens();

        CompletableFuture<Providers.MaxTokensResponse> replaceMaxTokens(
                Providers.MaxTokensBudgets budgets);
    }

    public interface SessionsPlane {
        CompletableFuture<SessionsApi.CreateSessionResult> create(String runbookName);

        CompletableFuture<SessionsApi.TurnResult> turn(String sessionId, Params.TurnOptions options);

        /**
         * Streamed turn: {@code onProgress} fires on the offload thread as
         * stage events land; the future completes with the terminal result
         * (or the typed exception a mid-stream error decodes to).
         */
        CompletableFuture<SessionsApi.TurnResult> turnStream(
                String sessionId, Params.TurnOptions options,
                Consumer<SessionsApi.TurnProgress> onProgress);

        CompletableFuture<SessionsApi.Session> get(String sessionId);

        CompletableFuture<SessionsApi.Session> close(String sessionId);
    }

    public interface AccessTokensPlane {
        CompletableFuture<Tokens.TokenGrant> mint(Tokens.IssueTokenRequest request);

        CompletableFuture<List<Tokens.TokenInfo>> list(Params.TokenListQuery query);

        CompletableFuture<Tokens.RevokeResult> revoke(String jti);
    }

    public interface ReportsPlane {
        CompletableFuture<Reports.UsageReport> usage(Params.UsageQuery query);

        CompletableFuture<Reports.AuditPage> audit(Params.AuditQuery query);

        CompletableFuture<Reports.CostReport> cost(String from, String to);

        CompletableFuture<Reports.TimeseriesReport> timeseries(String window, String plane);

        CompletableFuture<Reports.EndpointsReport> endpoints(String window, Long limit);

        CompletableFuture<Reports.RunbookReport> runbooks(String window);

        CompletableFuture<Reports.SessionsReport> sessions(String window);

        CompletableFuture<Reports.EvidenceReport> evidenceReport(String window);

        CompletableFuture<Reports.MatrixReport> matrix();
    }

    /**
     * Async twin of {@link Planes.EvidencePlane} — sealed evidence READS
     *. REST-only; the gRPC transport completes exceptionally with
     * {@code UnsupportedTransportException}.
     */
    public interface EvidencePlane {
        CompletableFuture<JsonNode> evidence(String evidenceId);

        CompletableFuture<Evidence.EvidenceRows> evidenceRows(
                String evidenceId, Params.EvidenceRowsQuery q);
    }

    public interface AuthoringPlane {
        CompletableFuture<Authoring.PatternPage> listPatterns();

        CompletableFuture<Authoring.PatternDetail> getPattern(String id);

        CompletableFuture<Authoring.Draft> createDraft(Authoring.CreateDraftRequest request);

        CompletableFuture<Authoring.DraftPage> listDrafts();

        CompletableFuture<Authoring.Draft> getDraft(String draftId);

        CompletableFuture<Authoring.DraftDelete> deleteDraft(String draftId);

        CompletableFuture<Authoring.Draft> putAnswers(
                String draftId, JsonNode answers, boolean materialize);

        CompletableFuture<Authoring.DraftValidation> validate(String draftId);

        CompletableFuture<Authoring.AssistResult> assist(
                String draftId, Authoring.AssistRequest request);

        CompletableFuture<Authoring.ExportBundle> export(String draftId);

        CompletableFuture<Authoring.ApplyDraftResult> apply(String draftId);
    }
}
