// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client;

import com.fasterxml.jackson.databind.JsonNode;
import io.ioka.munarium.client.model.Authoring;
import io.ioka.munarium.client.model.Evidence;
import io.ioka.munarium.client.model.Ingesting;
import io.ioka.munarium.client.model.Ledger;
import io.ioka.munarium.client.model.Memory;
import io.ioka.munarium.client.model.Meta;
import io.ioka.munarium.client.model.Providers;
import io.ioka.munarium.client.model.Reports;
import io.ioka.munarium.client.model.Retrieval;
import io.ioka.munarium.client.model.Runbooks;
import io.ioka.munarium.client.model.SessionsApi;
import io.ioka.munarium.client.model.Tokens;
import io.ioka.munarium.client.planes.AsyncPlanes;
import io.ioka.munarium.client.planes.Params;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.function.Consumer;
import java.util.function.Supplier;

/**
 * The {@code CompletableFuture} facade. One implementation, zero drift: each
 * call offloads the corresponding {@link MunariumClient} method to a VIRTUAL
 * thread — on Java 21 blocking is the scalable primitive, so this is the
 * same trade the Python client made for its async gRPC, made explicit.
 * Futures fail with the library's typed
 * {@link io.ioka.munarium.client.errors.MunariumException} subclasses (wrapped in
 * {@code CompletionException} per {@code CompletableFuture} convention).
 */
public final class AsyncMunariumClient implements AutoCloseable {
    public final AsyncPlanes.CommandsPlane commands;
    public final AsyncPlanes.QueryPlane query;
    public final AsyncPlanes.IngestPlane ingest;
    public final AsyncPlanes.RetrievalPlane retrieval;
    public final AsyncPlanes.RunbooksPlane runbooks;
    public final AsyncPlanes.ProvidersPlane providers;
    public final AsyncPlanes.SessionsPlane sessions;
    public final AsyncPlanes.AccessTokensPlane tokens;
    public final AsyncPlanes.ReportsPlane reports;
    public final AsyncPlanes.AuthoringPlane authoring;

    /** Sealed evidence READS (REST-only). */
    public final AsyncPlanes.EvidencePlane evidence;

    private final MunariumClient sync;
    private final ExecutorService executor;

    private AsyncMunariumClient(MunariumClient sync) {
        this.sync = sync;
        this.executor = Executors.newVirtualThreadPerTaskExecutor();
        this.commands = new Commands();
        this.query = new Query();
        this.ingest = new Ingest();
        this.retrieval = new RetrievalP();
        this.runbooks = new RunbooksP();
        this.providers = new ProvidersP();
        this.sessions = new Sessions();
        this.tokens = new TokensP();
        this.reports = new ReportsP();
        this.authoring = new AuthoringP();
        this.evidence = new EvidenceP();
    }

    public static AsyncMunariumClient rest(MunariumClientOptions options) {
        return new AsyncMunariumClient(MunariumClient.rest(options));
    }

    public static AsyncMunariumClient grpc(MunariumClientOptions options) {
        return new AsyncMunariumClient(MunariumClient.grpc(options));
    }

    /** The underlying synchronous client (shared connection + auth). */
    public MunariumClient sync() {
        return sync;
    }

    public CompletableFuture<Meta.ServerVersion> serverVersion() {
        return supply(sync::serverVersion);
    }

    /**
     * The head-conflict write loop, async: the same re-read → rebuild →
     * retry as {@link MunariumClient#proposeClaimWithRetry} — its backoff
     * sleeps on the offload VIRTUAL thread, never the caller's.
     */
    public CompletableFuture<Ledger.ClaimOutcome> proposeClaimWithRetry(
            String versionId, java.util.function.LongFunction<Ledger.ClaimInput> build,
            int maxAttempts) {
        return supply(() -> sync.proposeClaimWithRetry(versionId, build, maxAttempts));
    }

    public CompletableFuture<Ledger.ClaimOutcome> proposeClaimWithRetry(
            String versionId, java.util.function.LongFunction<Ledger.ClaimInput> build) {
        return proposeClaimWithRetry(versionId, build, 3);
    }

    @Override
    public void close() {
        // Drain before teardown: in-flight offloaded calls must not race a
        // closing transport (a queued task on a closed channel/scheduler
        // would fail with an UNTYPED error, breaking the futures contract).
        executor.shutdown();
        try {
            if (!executor.awaitTermination(5, java.util.concurrent.TimeUnit.SECONDS)) {
                executor.shutdownNow();
            }
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            executor.shutdownNow();
        }
        sync.close();
    }

    private <T> CompletableFuture<T> supply(Supplier<T> call) {
        return CompletableFuture.supplyAsync(call, executor);
    }

    private CompletableFuture<Void> run(Runnable call) {
        return CompletableFuture.runAsync(call, executor);
    }

    private final class Commands implements AsyncPlanes.CommandsPlane {
        @Override
        public CompletableFuture<String> createVersion(String parent, JsonNode metadata, String idem) {
            return supply(() -> sync.commands.createVersion(parent, metadata, idem));
        }

        @Override
        public CompletableFuture<Ledger.ClaimOutcome> proposeClaim(
                String versionId, Ledger.ClaimInput claim, Long expectedHead, String idem) {
            return supply(() -> sync.commands.proposeClaim(versionId, claim, expectedHead, idem));
        }

        @Override
        public CompletableFuture<Ledger.EventsOutcome> appendEvents(String versionId,
                List<Ledger.ClaimInput> claims, String candidateText, Long expectedHead, String idem) {
            return supply(() ->
                    sync.commands.appendEvents(versionId, claims, candidateText, expectedHead, idem));
        }

        @Override
        public CompletableFuture<Memory.Promise> openPromise(
                String versionId, Params.PromiseInput promise, String idem) {
            return supply(() -> sync.commands.openPromise(versionId, promise, idem));
        }

        @Override
        public CompletableFuture<Boolean> fulfillPromise(String versionId, String key, String idem) {
            return supply(() -> sync.commands.fulfillPromise(versionId, key, idem));
        }

        @Override
        public CompletableFuture<Memory.Anchor> lockAnchor(
                String versionId, Params.AnchorInput anchor, String idem) {
            return supply(() -> sync.commands.lockAnchor(versionId, anchor, idem));
        }

        @Override
        public CompletableFuture<Void> recordCounts(String versionId, String key, String scopePath,
                long count, Long budget, String idem) {
            return run(() -> sync.commands.recordCounts(versionId, key, scopePath, count, budget, idem));
        }

        @Override
        public CompletableFuture<Void> upsertDigest(Memory.Digest digest) {
            return run(() -> sync.commands.upsertDigest(digest));
        }
    }

    private final class Query implements AsyncPlanes.QueryPlane {
        @Override
        public CompletableFuture<Long> head(String versionId) {
            return supply(() -> sync.query.head(versionId));
        }

        @Override
        public CompletableFuture<Ledger.ClaimLookup> getClaim(String claimId) {
            return supply(() -> sync.query.getClaim(claimId));
        }

        @Override
        public CompletableFuture<Ledger.FactsPage> facts(String versionId, Params.FactsQuery q) {
            return supply(() -> sync.query.facts(versionId, q));
        }

        @Override
        public CompletableFuture<List<String>> lineage(String versionId) {
            return supply(() -> sync.query.lineage(versionId));
        }

        @Override
        public CompletableFuture<List<Memory.Anchor>> anchors(String versionId, Long asOfSeq) {
            return supply(() -> sync.query.anchors(versionId, asOfSeq));
        }

        @Override
        public CompletableFuture<List<Memory.Promise>> promises(
                String versionId, Long asOfSeq, String status) {
            return supply(() -> sync.query.promises(versionId, asOfSeq, status));
        }

        @Override
        public CompletableFuture<List<Memory.Counter>> counters(String versionId, Long asOfSeq) {
            return supply(() -> sync.query.counters(versionId, asOfSeq));
        }

        @Override
        public CompletableFuture<List<Memory.Digest>> digests(String versionId) {
            return supply(() -> sync.query.digests(versionId));
        }

        @Override
        public CompletableFuture<List<Ledger.StoredFinding>> findings(
                String versionId, Params.FindingsQuery q) {
            return supply(() -> sync.query.findings(versionId, q));
        }

        @Override
        public CompletableFuture<Memory.ComposedContext> composeContext(
                String versionId, Params.ContextQuery q) {
            return supply(() -> sync.query.composeContext(versionId, q));
        }
    }

    private final class Ingest implements AsyncPlanes.IngestPlane {
        @Override
        public CompletableFuture<Ingesting.PutSourceResult> putSource(
                Params.ChunkSource data, Params.SourceMeta meta) {
            return supply(() -> sync.ingest.putSource(data, meta));
        }

        @Override
        public CompletableFuture<Ingesting.RecordIngestResult> recordIngest(
                String versionId, String contentHash, String shapeRef) {
            return supply(() -> sync.ingest.recordIngest(versionId, contentHash, shapeRef));
        }

        @Override
        public CompletableFuture<Ingesting.IngestResult> ingest(Ingesting.IngestFile file) {
            return supply(() -> sync.ingest.ingest(file));
        }

        @Override
        public CompletableFuture<List<Ingesting.IngestResult>> ingestBatch(
                List<Ingesting.IngestFile> files) {
            return supply(() -> sync.ingest.ingestBatch(files));
        }

        @Override
        public CompletableFuture<Ingesting.BulkOpenResult> bulkOpen(
                List<Ingesting.BulkManifestEntry> files, String label) {
            return supply(() -> sync.ingest.bulkOpen(files, label));
        }

        @Override
        public CompletableFuture<Ingesting.BulkChunkResult> bulkChunk(
                String bulkId, List<Ingesting.IngestFile> files) {
            return supply(() -> sync.ingest.bulkChunk(bulkId, files));
        }

        @Override
        public CompletableFuture<Ingesting.BulkStatus> bulkStatus(
                String bulkId, boolean includeNeeded) {
            return supply(() -> sync.ingest.bulkStatus(bulkId, includeNeeded));
        }

        @Override
        public CompletableFuture<Ingesting.BulkCompleteResult> bulkComplete(String bulkId) {
            return supply(() -> sync.ingest.bulkComplete(bulkId));
        }

        @Override
        public CompletableFuture<Ingesting.SourceInfo> getSource(String sourceId) {
            return supply(() -> sync.ingest.getSource(sourceId));
        }
    }

    private final class RetrievalP implements AsyncPlanes.RetrievalPlane {
        @Override
        public CompletableFuture<Retrieval.SearchResult> search(Params.SearchQuery q) {
            return supply(() -> sync.retrieval.search(q));
        }

        @Override
        public CompletableFuture<Retrieval.IndexStatus> indexStatus(String shapeRef) {
            return supply(() -> sync.retrieval.indexStatus(shapeRef));
        }

        @Override
        public CompletableFuture<Retrieval.IndexStatus> buildIndex(String shapeRef, String versionId) {
            return supply(() -> sync.retrieval.buildIndex(shapeRef, versionId));
        }

        @Override
        public CompletableFuture<Retrieval.CollectionInfo> createCollection(
                Params.CollectionSpec spec) {
            return supply(() -> sync.retrieval.createCollection(spec));
        }

        @Override
        public CompletableFuture<List<Retrieval.CollectionInfo>> listCollections() {
            return supply(sync.retrieval::listCollections);
        }

        @Override
        public CompletableFuture<Retrieval.CollectionInfo> getCollection(String id) {
            return supply(() -> sync.retrieval.getCollection(id));
        }
    }

    private final class RunbooksP implements AsyncPlanes.RunbooksPlane {
        @Override
        public CompletableFuture<Runbooks.ApplyShapeResult> applyShape(String yaml, String versionId) {
            return supply(() -> sync.runbooks.applyShape(yaml, versionId));
        }

        @Override
        public CompletableFuture<String> applyRunbook(String yaml) {
            return supply(() -> sync.runbooks.applyRunbook(yaml));
        }

        @Override
        public CompletableFuture<Runbooks.RunbookRun> runRunbook(String name, String versionId) {
            return supply(() -> sync.runbooks.runRunbook(name, versionId));
        }

        @Override
        public CompletableFuture<Runbooks.RunStatus> getRun(String runId) {
            return supply(() -> sync.runbooks.getRun(runId));
        }

        @Override
        public CompletableFuture<Runbooks.RunbookRun> approveStep(String runId, int ordinal) {
            return supply(() -> sync.runbooks.approveStep(runId, ordinal));
        }

        @Override
        public CompletableFuture<List<Runbooks.RunbookSummary>> list(boolean includeRemoved) {
            return supply(() -> sync.runbooks.list(includeRemoved));
        }

        @Override
        public CompletableFuture<Runbooks.RunbookInfo> getInfo(String name) {
            return supply(() -> sync.runbooks.getInfo(name));
        }

        @Override
        public CompletableFuture<Runbooks.ValidateResult> validate(
                String yaml, Params.ValidateOptions options) {
            return supply(() -> sync.runbooks.validate(yaml, options));
        }

        @Override
        public CompletableFuture<Runbooks.RemovalRequest> removeRequest(String name) {
            return supply(() -> sync.runbooks.removeRequest(name));
        }

        @Override
        public CompletableFuture<Runbooks.RemovalConfirm> removeConfirm(String name, String removalId) {
            return supply(() -> sync.runbooks.removeConfirm(name, removalId));
        }

        @Override
        public CompletableFuture<Runbooks.ChronologyRulesResult> applyChronologyRules(String yaml) {
            return supply(() -> sync.runbooks.applyChronologyRules(yaml));
        }

        @Override
        public CompletableFuture<String> getChronologyRules(String name) {
            return supply(() -> sync.runbooks.getChronologyRules(name));
        }
    }

    private final class ProvidersP implements AsyncPlanes.ProvidersPlane {
        @Override
        public CompletableFuture<String> applyConfig(String yaml) {
            return supply(() -> sync.providers.applyConfig(yaml));
        }

        @Override
        public CompletableFuture<Providers.ProviderHealth> health(String name) {
            return supply(() -> sync.providers.health(name));
        }

        @Override
        public CompletableFuture<Providers.HealthAiResult> healthAi() {
            return supply(sync.providers::healthAi);
        }

        @Override
        public CompletableFuture<Providers.CompleteResult> complete(
                String name, Params.CompleteOptions options) {
            return supply(() -> sync.providers.complete(name, options));
        }

        @Override
        public CompletableFuture<Providers.EmbedResult> embed(String name, Params.EmbedOptions options) {
            return supply(() -> sync.providers.embed(name, options));
        }

        @Override
        public CompletableFuture<Providers.ProviderList> list() {
            return supply(sync.providers::list);
        }

        @Override
        public CompletableFuture<Providers.MaxTokensResponse> maxTokens() {
            return supply(sync.providers::maxTokens);
        }

        @Override
        public CompletableFuture<Providers.MaxTokensResponse> replaceMaxTokens(
                Providers.MaxTokensBudgets budgets) {
            return supply(() -> sync.providers.replaceMaxTokens(budgets));
        }
    }

    private final class Sessions implements AsyncPlanes.SessionsPlane {
        @Override
        public CompletableFuture<SessionsApi.CreateSessionResult> create(String runbookName) {
            return supply(() -> sync.sessions.create(runbookName));
        }

        @Override
        public CompletableFuture<SessionsApi.TurnResult> turn(
                String sessionId, Params.TurnOptions options) {
            return supply(() -> sync.sessions.turn(sessionId, options));
        }

        @Override
        public CompletableFuture<SessionsApi.TurnResult> turnStream(String sessionId,
                Params.TurnOptions options, Consumer<SessionsApi.TurnProgress> onProgress) {
            return supply(() -> sync.sessions.turnStream(sessionId, options, onProgress));
        }

        @Override
        public CompletableFuture<SessionsApi.Session> get(String sessionId) {
            return supply(() -> sync.sessions.get(sessionId));
        }

        @Override
        public CompletableFuture<SessionsApi.Session> close(String sessionId) {
            return supply(() -> sync.sessions.close(sessionId));
        }
    }

    private final class TokensP implements AsyncPlanes.AccessTokensPlane {
        @Override
        public CompletableFuture<Tokens.TokenGrant> mint(Tokens.IssueTokenRequest request) {
            return supply(() -> sync.tokens.mint(request));
        }

        @Override
        public CompletableFuture<List<Tokens.TokenInfo>> list(Params.TokenListQuery query) {
            return supply(() -> sync.tokens.list(query));
        }

        @Override
        public CompletableFuture<Tokens.RevokeResult> revoke(String jti) {
            return supply(() -> sync.tokens.revoke(jti));
        }
    }

    private final class ReportsP implements AsyncPlanes.ReportsPlane {
        @Override
        public CompletableFuture<Reports.UsageReport> usage(Params.UsageQuery query) {
            return supply(() -> sync.reports.usage(query));
        }

        @Override
        public CompletableFuture<Reports.AuditPage> audit(Params.AuditQuery query) {
            return supply(() -> sync.reports.audit(query));
        }

        @Override
        public CompletableFuture<Reports.CostReport> cost(String from, String to) {
            return supply(() -> sync.reports.cost(from, to));
        }

        @Override
        public CompletableFuture<Reports.TimeseriesReport> timeseries(String window, String plane) {
            return supply(() -> sync.reports.timeseries(window, plane));
        }

        @Override
        public CompletableFuture<Reports.EndpointsReport> endpoints(String window, Long limit) {
            return supply(() -> sync.reports.endpoints(window, limit));
        }

        @Override
        public CompletableFuture<Reports.RunbookReport> runbooks(String window) {
            return supply(() -> sync.reports.runbooks(window));
        }

        @Override
        public CompletableFuture<Reports.SessionsReport> sessions(String window) {
            return supply(() -> sync.reports.sessions(window));
        }

        @Override
        public CompletableFuture<Reports.EvidenceReport> evidenceReport(String window) {
            return supply(() -> sync.reports.evidenceReport(window));
        }

        @Override
        public CompletableFuture<Reports.MatrixReport> matrix() {
            return supply(() -> sync.reports.matrix());
        }
    }

    private final class EvidenceP implements AsyncPlanes.EvidencePlane {
        @Override
        public CompletableFuture<JsonNode> evidence(String evidenceId) {
            return supply(() -> sync.evidence.evidence(evidenceId));
        }

        @Override
        public CompletableFuture<Evidence.EvidenceRows> evidenceRows(
                String evidenceId, Params.EvidenceRowsQuery q) {
            return supply(() -> sync.evidence.evidenceRows(evidenceId, q));
        }
    }

    private final class AuthoringP implements AsyncPlanes.AuthoringPlane {
        @Override
        public CompletableFuture<Authoring.PatternPage> listPatterns() {
            return supply(sync.authoring::listPatterns);
        }

        @Override
        public CompletableFuture<Authoring.PatternDetail> getPattern(String id) {
            return supply(() -> sync.authoring.getPattern(id));
        }

        @Override
        public CompletableFuture<Authoring.Draft> createDraft(Authoring.CreateDraftRequest request) {
            return supply(() -> sync.authoring.createDraft(request));
        }

        @Override
        public CompletableFuture<Authoring.DraftPage> listDrafts() {
            return supply(sync.authoring::listDrafts);
        }

        @Override
        public CompletableFuture<Authoring.Draft> getDraft(String draftId) {
            return supply(() -> sync.authoring.getDraft(draftId));
        }

        @Override
        public CompletableFuture<Authoring.DraftDelete> deleteDraft(String draftId) {
            return supply(() -> sync.authoring.deleteDraft(draftId));
        }

        @Override
        public CompletableFuture<Authoring.Draft> putAnswers(
                String draftId, JsonNode answers, boolean materialize) {
            return supply(() -> sync.authoring.putAnswers(draftId, answers, materialize));
        }

        @Override
        public CompletableFuture<Authoring.DraftValidation> validate(String draftId) {
            return supply(() -> sync.authoring.validate(draftId));
        }

        @Override
        public CompletableFuture<Authoring.AssistResult> assist(
                String draftId, Authoring.AssistRequest request) {
            return supply(() -> sync.authoring.assist(draftId, request));
        }

        @Override
        public CompletableFuture<Authoring.ExportBundle> export(String draftId) {
            return supply(() -> sync.authoring.export(draftId));
        }

        @Override
        public CompletableFuture<Authoring.ApplyDraftResult> apply(String draftId) {
            return supply(() -> sync.authoring.apply(draftId));
        }
    }
}
