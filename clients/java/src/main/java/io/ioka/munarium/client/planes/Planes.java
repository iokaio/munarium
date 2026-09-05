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
import java.util.function.Consumer;

/**
 * The ten SYNCHRONOUS plane interfaces — one surface, two transports.
 * {@link AsyncPlanes} holds the {@code CompletableFuture} twins with
 * identical semantics. Every method throws a subclass of
 * {@link io.ioka.munarium.client.errors.MunariumException} on failure; an
 * operation with no route/RPC on the constructed transport throws the typed
 * {@link io.ioka.munarium.client.errors.UnsupportedTransportException} —
 * honestly, never a silent drop.
 */
public final class Planes {
    private Planes() {}

    /** Writes through the deterministic gates (auto idempotency keys). */
    public interface CommandsPlane {
        String createVersion(String parentVersionId, JsonNode metadata, String idempotencyKey);

        default String createVersion() {
            return createVersion(null, null, null);
        }

        /**
         * Propose one claim. A gate-blocked claim is NOT an error: the
         * outcome carries {@code status == "disputed"} plus the findings
         * (recorded, never dropped). Only an {@code expectedHead} mismatch
         * throws ({@link io.ioka.munarium.client.errors.HeadConflictException}).
         */
        Ledger.ClaimOutcome proposeClaim(
                String versionId, Ledger.ClaimInput claim, Long expectedHead, String idempotencyKey);

        /** Batched claims, gated as ONE candidate unit. */
        Ledger.EventsOutcome appendEvents(
                String versionId,
                List<Ledger.ClaimInput> claims,
                String candidateText,
                Long expectedHead,
                String idempotencyKey);

        Memory.Promise openPromise(String versionId, Params.PromiseInput promise, String idempotencyKey);

        boolean fulfillPromise(String versionId, String key, String idempotencyKey);

        Memory.Anchor lockAnchor(String versionId, Params.AnchorInput anchor, String idempotencyKey);

        void recordCounts(
                String versionId,
                String key,
                String scopePath,
                long count,
                Long budget,
                String idempotencyKey);

        /** Upsert by definition — the one command outside idempotency scope. */
        void upsertDigest(Memory.Digest digest);
    }

    /**
     * Point-in-time reads. One {@code asOfSeq} pin bounds facts, anchors,
     * promises, and counters together; digests are rebuilt under a pin.
     */
    public interface QueryPlane {
        long head(String versionId);

        Ledger.ClaimLookup getClaim(String claimId);

        Ledger.FactsPage facts(String versionId, Params.FactsQuery query);

        List<String> lineage(String versionId);

        List<Memory.Anchor> anchors(String versionId, Long asOfSeq);

        List<Memory.Promise> promises(String versionId, Long asOfSeq, String status);

        List<Memory.Counter> counters(String versionId, Long asOfSeq);

        List<Memory.Digest> digests(String versionId);

        /** Persisted gate findings (REST-only today). */
        List<Ledger.StoredFinding> findings(String versionId, Params.FindingsQuery query);

        Memory.ComposedContext composeContext(String versionId, Params.ContextQuery query);
    }

    /** Content-addressed source intake + the file/bulk planes. */
    public interface IngestPlane {
        /** Streamed upload; the source REPLAYS per attempt (see ChunkSource). */
        Ingesting.PutSourceResult putSource(Params.ChunkSource data, Params.SourceMeta meta);

        Ingesting.RecordIngestResult recordIngest(
                String versionId, String contentHash, String shapeRef);

        /** One document via the file plane (ingest scope required). */
        Ingesting.IngestResult ingest(Ingesting.IngestFile file);

        /** 1..=500 files, per-item outcomes — one bad file never fails the batch. */
        List<Ingesting.IngestResult> ingestBatch(List<Ingesting.IngestFile> files);

        /** Open a bulk session; {@code needed} is the upload work list. REST-only. */
        Ingesting.BulkOpenResult bulkOpen(List<Ingesting.BulkManifestEntry> files, String label);

        /** 1..=500 files per chunk (a larger list is a typed LOCAL error). REST-only. */
        Ingesting.BulkChunkResult bulkChunk(String bulkId, List<Ingesting.IngestFile> files);

        Ingesting.BulkStatus bulkStatus(String bulkId, boolean includeNeeded);

        Ingesting.BulkCompleteResult bulkComplete(String bulkId);

        /** Metadata for one stored source (never the bytes). REST-only. */
        Ingesting.SourceInfo getSource(String sourceId);
    }

    /** Hybrid search over versioned immutable indexes + collections. */
    public interface RetrievalPlane {
        Retrieval.SearchResult search(Params.SearchQuery query);

        Retrieval.IndexStatus indexStatus(String shapeRef);

        /** Side-by-side build + atomic flip. REST-only. */
        Retrieval.IndexStatus buildIndex(String shapeRef, String versionId);

        Retrieval.CollectionInfo createCollection(Params.CollectionSpec spec);

        List<Retrieval.CollectionInfo> listCollections();

        Retrieval.CollectionInfo getCollection(String id);
    }

    /** Shapes + runbooks: v1 runs and the v2 surface. */
    public interface RunbooksPlane {
        Runbooks.ApplyShapeResult applyShape(String yaml, String versionId);

        String applyRunbook(String yaml);

        Runbooks.RunbookRun runRunbook(String name, String versionId);

        Runbooks.RunStatus getRun(String runId);

        Runbooks.RunbookRun approveStep(String runId, int ordinal);

        List<Runbooks.RunbookSummary> list(boolean includeRemoved);

        Runbooks.RunbookInfo getInfo(String name);

        Runbooks.ValidateResult validate(String yaml, Params.ValidateOptions options);

        /** First pass of the double-pass soft removal (EXACT name@version). */
        Runbooks.RemovalRequest removeRequest(String name);

        Runbooks.RemovalConfirm removeConfirm(String name, String removalId);

        /** The sixth gate's arming surface. REST-only. */
        Runbooks.ChronologyRulesResult applyChronologyRules(String yaml);

        /** The applied rules YAML back, verbatim. REST-only. */
        String getChronologyRules(String name);
    }

    /** The BYOK provider gateway. */
    public interface ProvidersPlane {
        String applyConfig(String yaml);

        Providers.ProviderHealth health(String name);

        /** Live six-model default probe — spends provider tokens. REST-only. */
        Providers.HealthAiResult healthAi();

        Providers.CompleteResult complete(String name, Params.CompleteOptions options);

        Providers.EmbedResult embed(String name, Params.EmbedOptions options);

        /** Free disclosure of tenant-visible configs + tiers. REST-only. */
        Providers.ProviderList list();

        /**
         * {@code GET /v1/max-tokens} — the effective per-call output-token
         * budgets for the caller's tenant and where they come from
         * ({@code source}: {@code tenant} after a replacement, else
         * {@code environment}). Any authenticated role: the numbers shape
         * spend, they are not secrets. REST-only.
         */
        Providers.MaxTokensResponse maxTokens();

        /**
         * {@code POST /v1/max-tokens} — replace the tenant's WHOLE set.
         * There is no partial update: every member of {@code budgets} is
         * sent, each range-checked server-side ({@code invalid-input} on a
         * miss — see {@link Providers.MaxTokensBudgets}), and the answer is
         * the same shape {@link #maxTokens()} returns. Static rw role only
         * ({@code forbidden} otherwise), like provider configs and
         * runbooks. REST-only.
         */
        Providers.MaxTokensResponse replaceMaxTokens(Providers.MaxTokensBudgets budgets);
    }

    /** Multiturn sessions over a runbook's access-permitted collections. */
    public interface SessionsPlane {
        SessionsApi.CreateSessionResult create(String runbookName);

        /**
         * One retrieval turn (+ optional completion). Deadline-exempt and
         * never auto-retried: a turn spends provider tokens a client-side
         * abort cannot stop.
         */
        SessionsApi.TurnResult turn(String sessionId, Params.TurnOptions options);

        /**
         * The same turn, streamed: {@code onProgress} fires per stage event
         * (retrieval/merge/model/completion/verify — informational; unknown
         * stages from a newer server still flow), and the full result
         * returns when the terminal event lands. A mid-stream server error
         * throws the same typed exception the unary route would; a stream
         * that ends without a terminal event throws a typed transport error
         * — never a silent success. REST-only.
         */
        SessionsApi.TurnResult turnStream(
                String sessionId, Params.TurnOptions options,
                Consumer<SessionsApi.TurnProgress> onProgress);

        SessionsApi.Session get(String sessionId);

        /** Idempotent: closing a closed/expired session echoes its state. */
        SessionsApi.Session close(String sessionId);
    }

    /** Capability-token mint/audit/revoke (mgmt role). */
    public interface AccessTokensPlane {
        /** Token material is returned ONCE and never persisted server-side. */
        Tokens.TokenGrant mint(Tokens.IssueTokenRequest request);

        List<Tokens.TokenInfo> list(Params.TokenListQuery query);

        Tokens.RevokeResult revoke(String jti);
    }

    /** Management reports (mgmt role). REST-only. */
    public interface ReportsPlane {
        Reports.UsageReport usage(Params.UsageQuery query);

        Reports.AuditPage audit(Params.AuditQuery query);

        Reports.CostReport cost(String from, String to);

        Reports.TimeseriesReport timeseries(String window, String plane);

        Reports.EndpointsReport endpoints(String window, Long limit);

        Reports.RunbookReport runbooks(String window);

        Reports.SessionsReport sessions(String window);

        /**
         * How the evidence hierarchy behaved; {@code window} is
         * {@code 24h} (server default) | {@code 7d} | {@code 30d}.
         *
         * <p>Named {@code evidenceReport}, not {@code evidence}, because
         * {@link io.ioka.munarium.client.Transport} implements every plane on one type and
         * {@link EvidencePlane#evidence(String)} already owns that erasure —
         * a same-signature clash would be a compile error, and renaming the
         * artifact read would have been the worse break.
         */
        Reports.EvidenceReport evidenceReport(String window);

        /** Munarium Matrix's health as this server instance sees it. */
        Reports.MatrixReport matrix();
    }

    /**
     * Sealed evidence READS. REST-only; the gRPC transport throws
     * {@code UnsupportedTransportException}.
     *
     * <p>Sealing is deliberately absent. An artifact's manifest is a statement
     * about work the sealer did — a client offering {@code sealEvidence} would
     * invite an application to assert provenance it cannot vouch for. What an
     * application legitimately needs is the other direction: an answer cites
     * {@code [evidence/<id>#<row>]} and the application resolves that citation
     * to show a reader what the number was computed from.
     *
     * <p>Access is checked per artifact against the SESSION's clearance, not
     * the sealer's. Expect {@code evidence-forbidden} (403),
     * {@code evidence-expired} (410 — retention purged the bytes, and the
     * citation was real) and {@code evidence-not-committed} (409).
     */
    public interface EvidencePlane {
        /**
         * The manifest, verbatim as the contract defines it. Returned
         * UNWRAPPED by the route, so this is the manifest itself.
         */
        JsonNode evidence(String evidenceId);

        /** A bounded, audited window over the sealed rows. */
        Evidence.EvidenceRows evidenceRows(String evidenceId, Params.EvidenceRowsQuery q);
    }

    /**
     * Guided runbook authoring. REST-only. {@code deleteDraft} is the client
     * surface's ONE delete — workspace cleanup, never ledger data.
     */
    public interface AuthoringPlane {
        Authoring.PatternPage listPatterns();

        Authoring.PatternDetail getPattern(String id);

        Authoring.Draft createDraft(Authoring.CreateDraftRequest request);

        Authoring.DraftPage listDrafts();

        Authoring.Draft getDraft(String draftId);

        Authoring.DraftDelete deleteDraft(String draftId);

        Authoring.Draft putAnswers(String draftId, JsonNode answers, boolean materialize);

        Authoring.DraftValidation validate(String draftId);

        /** NEVER fails the request — a degraded pass sets {@code assistNote}. */
        Authoring.AssistResult assist(String draftId, Authoring.AssistRequest request);

        Authoring.ExportBundle export(String draftId);

        Authoring.ApplyDraftResult apply(String draftId);
    }
}
