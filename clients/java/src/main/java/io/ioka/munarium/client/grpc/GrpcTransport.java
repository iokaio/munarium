// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.grpc;

import com.fasterxml.jackson.databind.JsonNode;
import com.google.protobuf.Any;
import com.google.protobuf.ByteString;
import com.google.protobuf.InvalidProtocolBufferException;
import com.google.rpc.ErrorInfo;
import io.grpc.ManagedChannel;
import io.grpc.Channel;
import io.grpc.ClientInterceptors;
import io.grpc.ManagedChannelBuilder;
import io.grpc.Metadata;
import io.grpc.StatusRuntimeException;
import io.grpc.protobuf.StatusProto;
import io.grpc.stub.AbstractStub;
import io.grpc.stub.MetadataUtils;
import io.grpc.stub.StreamObserver;
import io.ioka.munarium.client.MunariumClientOptions;
import io.ioka.munarium.client.errors.HeadConflictException;
import io.ioka.munarium.client.errors.InvalidInputException;
import io.ioka.munarium.client.errors.MunariumException;
import io.ioka.munarium.client.errors.MunariumTransportException;
import io.ioka.munarium.client.errors.NotFoundException;
import io.ioka.munarium.client.errors.Problems;
import io.ioka.munarium.client.errors.ProviderException;
import io.ioka.munarium.client.errors.RateLimitedException;
import io.ioka.munarium.client.errors.StorageException;
import io.ioka.munarium.client.errors.UnauthenticatedException;
import io.ioka.munarium.client.errors.ForbiddenException;
import io.ioka.munarium.client.errors.UnexpectedServerException;
import io.ioka.munarium.client.errors.UnsupportedTransportException;
import io.ioka.munarium.client.model.Authoring;
import io.ioka.munarium.client.model.Ingesting;
import io.ioka.munarium.client.model.Json;
import io.ioka.munarium.client.model.Evidence;
import io.ioka.munarium.client.model.Ledger;
import io.ioka.munarium.client.model.Memory;
import io.ioka.munarium.client.model.Providers;
import io.ioka.munarium.client.model.Reports;
import io.ioka.munarium.client.model.Retrieval;
import io.ioka.munarium.client.model.Runbooks;
import io.ioka.munarium.client.model.SessionsApi;
import io.ioka.munarium.client.model.Tokens;
import io.ioka.munarium.client.planes.Params;
import io.ioka.munarium.client.planes.Planes;
import io.ioka.munarium.client.internal.Wire;
import java.io.IOException;
import java.io.InputStream;
import java.util.ArrayList;
import java.util.Base64;
import java.util.List;
import java.util.UUID;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicReference;
import java.util.function.Consumer;
import java.util.function.Supplier;
import mmp.v1.Admin;
import mmp.v1.AdminServiceGrpc;
import mmp.v1.Command;
import mmp.v1.CommandServiceGrpc;
import mmp.v1.Common;
import mmp.v1.Ingest;
import mmp.v1.IngestServiceGrpc;
import mmp.v1.Ledger.Anchor;
import mmp.v1.Ledger.Claim;
import mmp.v1.Ledger.ClaimType;
import mmp.v1.Ledger.ComposedContext;
import mmp.v1.Ledger.CounterState;
import mmp.v1.Ledger.Digest;
import mmp.v1.Ledger.Promise;
import mmp.v1.Ledger.Provenance;
import mmp.v1.Provider;
import mmp.v1.ProviderServiceGrpc;
import mmp.v1.Query;
import mmp.v1.QueryServiceGrpc;
import mmp.v1.Retrieval.CollectionInfo;
import mmp.v1.RetrievalServiceGrpc;
import mmp.v1.Runbook;
import mmp.v1.RunbookServiceGrpc;
import mmp.v1.Session;
import mmp.v1.SessionServiceGrpc;

/**
 * gRPC transport (netty-shaded) over the direct :50051 plane (or :443 via
 * the gateway). Errors decode the {@code google.rpc.ErrorInfo} structured
 * detail through the one {@link Problems} path; commands carry
 * auto-generated idempotency-key metadata and are re-sent ONLY for failures
 * that provably shed the request before execution (the typed
 * {@code overloaded}) — on gRPC no transport failure is provably
 * undelivered (a failed lazy reconnect and a broken established stream both
 * surface as UNAVAILABLE), so transport failures on commands surface to the
 * caller, matching the Python and .NET clients.
 *
 * <p>Transport notes (documented parity gaps, not bugs): the REST-only platform
 * surface throws the typed {@link UnsupportedTransportException} here —
 * {@code turnStream} (SSE), the four bulk-upload routes, {@code getSource},
 * {@code findings}, chronology rules, {@code providers.list}, the
 * max-tokens budgets pair ({@code maxTokens}/{@code replaceMaxTokens}),
 * {@code healthAi}, {@code buildIndex}, every reports method
 * (AdminService.Usage is declared but UNIMPLEMENTED — not wired), and the
 * whole authoring plane. proto3 scalars cannot carry "explicitly zero":
 * an explicit 0 for {@code asOfSeq}/{@code limit}/{@code topK}/
 * {@code factLimit}/{@code budgetTokens}/{@code maxTokens}/counter
 * {@code budget}/{@code ttlSecs}, or 0.0 for {@code confidence}/
 * {@code temperature}, is rejected as invalid input instead of silently
 * meaning "absent" (REST carries them faithfully).
 */
public final class GrpcTransport implements io.ioka.munarium.client.Transport {

    private static final Metadata.Key<String> AUTH =
            Metadata.Key.of("authorization", Metadata.ASCII_STRING_MARSHALLER);
    private static final Metadata.Key<String> UID =
            Metadata.Key.of("munarium-uid", Metadata.ASCII_STRING_MARSHALLER);
    private static final Metadata.Key<String> IDEM =
            Metadata.Key.of("idempotency-key", Metadata.ASCII_STRING_MARSHALLER);

    private static final int CHUNK_BYTES = 1024 * 1024;

    private final ManagedChannel managed;
    /** The managed channel with the constant auth/uid headers attached ONCE
     * — stubs are built from this, so per-call work is only the deadline
     * (and the idempotency key on commands). */
    private final Channel channel;
    private final MunariumClientOptions options;

    public GrpcTransport(MunariumClientOptions options) {
        String target = options.endpoint();
        boolean plaintext = !target.startsWith("https://");
        target = target.replaceFirst("^https?://", "").replaceAll("/+$", "");
        ManagedChannelBuilder<?> b = ManagedChannelBuilder.forTarget(target);
        if (plaintext) {
            // Plaintext exactly when the scheme is http:// or absent — the
            // direct dev plane; pass https:// for the TLS gateway.
            b.usePlaintext();
        }
        this.managed = b.build();
        this.options = options;
        this.channel = ClientInterceptors.intercept(
                managed, MetadataUtils.newAttachHeadersInterceptor(constantHeaders()));
    }

    private Metadata constantHeaders() {
        Metadata md = new Metadata();
        if (options.token() != null) {
            md.put(AUTH, "Bearer " + options.token());
        }
        if (options.uid() != null) {
            md.put(UID, options.uid());
        }
        return md;
    }

    @Override
    public void close() {
        managed.shutdown();
        try {
            managed.awaitTermination(2, TimeUnit.SECONDS);
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
        }
    }

    // -- call plumbing ------------------------------------------------------

    private <S extends AbstractStub<S>> S prepare(S stub, String idem, boolean deadline) {
        S s = stub;
        if (idem != null) {
            Metadata md = new Metadata();
            md.put(IDEM, idem);
            s = s.withInterceptors(MetadataUtils.newAttachHeadersInterceptor(md));
        }
        if (deadline) {
            s = s.withDeadlineAfter(options.requestTimeout().toMillis(), TimeUnit.MILLISECONDS);
        }
        return s;
    }

    static MunariumException decode(StatusRuntimeException e) {
        com.google.rpc.Status status = StatusProto.fromThrowable(e);
        String detail = e.getStatus().getDescription() == null ? "" : e.getStatus().getDescription();
        if (status != null) {
            for (Any any : status.getDetailsList()) {
                if (any.is(ErrorInfo.class)) {
                    try {
                        ErrorInfo info = any.unpack(ErrorInfo.class);
                        if ("mmp.ioka.io".equals(info.getDomain())) {
                            return Problems.fromGrpcInfo(info.getReason(), detail, info.getMetadataMap());
                        }
                    } catch (InvalidProtocolBufferException ignored) {
                        // fall through to the code-based mapping
                    }
                }
            }
        }
        return switch (e.getStatus().getCode()) {
            // ABORTED is the head-conflict code; without details the seqs
            // are unknown — 0/0 tells the write loop to re-read the head.
            case ABORTED -> new HeadConflictException(0, 0, detail);
            case NOT_FOUND -> new NotFoundException("resource", "", detail);
            case INVALID_ARGUMENT -> new InvalidInputException(detail);
            case UNAUTHENTICATED -> new UnauthenticatedException(detail);
            case PERMISSION_DENIED -> new ForbiddenException(detail);
            case RESOURCE_EXHAUSTED -> new RateLimitedException(detail, null);
            // UNAVAILABLE + both spellings of deadline expiry: transport
            // faults, and all may have reached the server.
            case UNAVAILABLE, CANCELLED, DEADLINE_EXCEEDED ->
                    new MunariumTransportException(detail, true);
            case INTERNAL -> new StorageException(detail);
            default -> new UnexpectedServerException(
                    e.getStatus().getCode() + ": " + detail, null);
        };
    }

    /** Read path: retry transient failures with backoff. */
    private <T> T read(Supplier<T> call) {
        return withRetry(call, true);
    }

    /**
     * Command path: same auto-key each attempt, re-sent only on the typed
     * pre-execution shed ({@code overloaded}) — see the class doc.
     */
    private <T> T command(Supplier<T> call) {
        return withRetry(call, false);
    }

    private <T> T withRetry(Supplier<T> call, boolean readClass) {
        int attempt = 0;
        while (true) {
            attempt++;
            try {
                return call.get();
            } catch (StatusRuntimeException sre) {
                MunariumException e = decode(sre);
                boolean retryable = readClass
                        ? e.isTransient()
                        : e instanceof io.ioka.munarium.client.errors.OverloadedException;
                if (retryable && attempt <= options.readRetries()) {
                    Wire.sleepBackoff(attempt);
                    continue;
                }
                throw e;
            }
        }
    }

    /** Send once, decode once — the non-replayable write class. */
    private <T> T once(Supplier<T> call) {
        try {
            return call.get();
        } catch (StatusRuntimeException e) {
            throw decode(e);
        }
    }

    private static void rejectZero(String name, Long v) {
        if (v != null && v == 0) {
            throw new InvalidInputException(name
                    + " = 0 cannot be represented on the gRPC wire (proto3 uses 0 for 'absent');"
                    + " omit it, or use the REST transport");
        }
    }

    private static void rejectZero(String name, Integer v) {
        rejectZero(name, v == null ? null : v.longValue());
    }

    private static String orEmpty(String s) {
        return s == null ? "" : s;
    }

    private static String optStr(String s) {
        return s == null || s.isEmpty() ? null : s;
    }

    private static JsonNode jsonOpt(String s) {
        if (s == null || s.isEmpty()) {
            return null;
        }
        try {
            return Json.MAPPER.readTree(s);
        } catch (IOException e) {
            return null;
        }
    }

    private static UnsupportedTransportException unsupported(String what, String route) {
        return new UnsupportedTransportException(
                what + " has no gRPC RPC today — use the REST client (" + route + ")");
    }

    // -- enum decode: governance values FAIL CLOSED (invariant #8) ----------

    private static String statusStr(Common.ClaimStatus s) {
        // An unknown/unset tag can never read as "the gates passed".
        return s == Common.ClaimStatus.CLAIM_STATUS_ACCEPTED ? "accepted" : "disputed";
    }

    private static String severityStr(Common.Severity s) {
        return switch (s) {
            case SEVERITY_INFO -> "info";
            case SEVERITY_WARN -> "warn";
            default -> "block";
        };
    }

    private static String provenanceStr(Provenance p) {
        return switch (p) {
            case PROVENANCE_WITNESSED -> "witnessed";
            case PROVENANCE_BACKFILLED -> "backfilled";
            case PROVENANCE_REPAIRED -> "repaired";
            case PROVENANCE_COVERAGE_REPAIR -> "coverage_repair";
            default -> "emergent";
        };
    }

    private static String claimTypeStr(ClaimType t) {
        return switch (t) {
            case CLAIM_TYPE_UPDATE -> "update";
            case CLAIM_TYPE_CORRECTION -> "correction";
            default -> "fact";
        };
    }

    private static ClaimType claimTypePb(String t) {
        return switch (t == null ? "fact" : t) {
            case "fact" -> ClaimType.CLAIM_TYPE_FACT;
            case "update" -> ClaimType.CLAIM_TYPE_UPDATE;
            case "correction" -> ClaimType.CLAIM_TYPE_CORRECTION;
            default -> throw new InvalidInputException(
                    "claim_type must be fact|update|correction, got '" + t + "'");
        };
    }

    private static Provenance provenancePb(String p) {
        if (p == null) {
            return Provenance.PROVENANCE_UNSPECIFIED;
        }
        return switch (p) {
            case "witnessed" -> Provenance.PROVENANCE_WITNESSED;
            case "backfilled" -> Provenance.PROVENANCE_BACKFILLED;
            case "repaired" -> Provenance.PROVENANCE_REPAIRED;
            case "emergent" -> Provenance.PROVENANCE_EMERGENT;
            case "coverage_repair" -> Provenance.PROVENANCE_COVERAGE_REPAIR;
            default -> throw new InvalidInputException("unknown provenance '" + p + "'");
        };
    }

    // -- pb -> model conversions --------------------------------------------

    private static Ledger.GateFinding finding(Common.GateFinding f) {
        return new Ledger.GateFinding(
                f.getRuleId(), severityStr(f.getSeverity()), f.getMessage(),
                optStr(f.getScopePath()), jsonOpt(f.getDetailJson()));
    }

    private static List<Ledger.GateFinding> findings(List<Common.GateFinding> fs) {
        return fs.stream().map(GrpcTransport::finding).toList();
    }

    private static Ledger.Claim claim(Claim c) {
        return new Ledger.Claim(
                c.getId(), c.getVersionId(), c.getSeq(), claimTypeStr(c.getClaimType()),
                c.getSubject(), c.getKey(), c.getValue(), c.getNormalizedText(),
                optStr(c.getScopePath()), statusStr(c.getStatus()),
                provenanceStr(c.getProvenance()), optStr(c.getSupersedesId()),
                optStr(c.getEntityId()), jsonOpt(c.getEvidenceJson()),
                c.getConfidence() == 0.0 ? null : c.getConfidence(), optStr(c.getShapeRef()),
                // A message field has presence: hasOrigin() is the honest "absent".
                c.hasOrigin() ? origin(c.getOrigin()) : null);
    }

    private static Ledger.ClaimOrigin origin(mmp.v1.Ledger.ClaimOrigin o) {
        return new Ledger.ClaimOrigin(
                o.getKind(), o.getSourceId(), o.getMappingVersion(), o.getRowKey(),
                optStr(o.getEventPosition()), optStr(o.getObservedAt()),
                optStr(o.getEvidenceId()));
    }

    private static mmp.v1.Ledger.ClaimOrigin originPb(Ledger.ClaimOrigin o) {
        return mmp.v1.Ledger.ClaimOrigin.newBuilder()
                .setKind(o.kind())
                .setSourceId(o.sourceId())
                .setMappingVersion(o.mappingVersion())
                .setRowKey(o.rowKey())
                .setEventPosition(orEmpty(o.eventPosition()))
                .setObservedAt(orEmpty(o.observedAt()))
                .setEvidenceId(orEmpty(o.evidenceId()))
                .build();
    }

    private static Memory.Anchor anchor(Anchor a) {
        return new Memory.Anchor(a.getId(), a.getVersionId(), a.getDetailKey(),
                a.getLockedValue(), optStr(a.getLockedAtScope()), a.getStatus(), a.getSeq());
    }

    private static Memory.Promise promise(Promise p) {
        return new Memory.Promise(p.getId(), p.getVersionId(), p.getKey(), p.getKind(),
                p.getDescription(), optStr(p.getOriginScope()), optStr(p.getDueScope()),
                p.getStatus(), p.getSeq(), p.getFulfilledSeq() == 0 ? null : p.getFulfilledSeq());
    }

    private static Retrieval.CollectionInfo collection(CollectionInfo c) {
        return new Retrieval.CollectionInfo(c.getId(), c.getName(), c.getShapeRef(),
                c.getAccessLevel(), c.getCompartmentsList(), c.getStatus(),
                optStr(c.getDescription()), c.getCreatedAt(), c.getSourceCount(),
                optStr(c.getActiveIndex()));
    }

    private static Retrieval.ProvenanceEnvelope envelope(Common.ProvenanceEnvelope e) {
        return new Retrieval.ProvenanceEnvelope(
                e.getChunkIdsList(), e.getSourceIdsList(), e.getSourcePathsList(),
                e.getSourceContentHashesList(), e.getIndexVersion(), e.getEventWatermark(),
                optStr(e.getProviderFingerprint()));
    }

    // -- stubs --------------------------------------------------------------

    private CommandServiceGrpc.CommandServiceBlockingStub commands(String idem) {
        return prepare(CommandServiceGrpc.newBlockingStub(channel), idem, true);
    }

    private QueryServiceGrpc.QueryServiceBlockingStub queries() {
        return prepare(QueryServiceGrpc.newBlockingStub(channel), null, true);
    }

    private IngestServiceGrpc.IngestServiceBlockingStub ingestSvc() {
        return prepare(IngestServiceGrpc.newBlockingStub(channel), null, true);
    }

    private RetrievalServiceGrpc.RetrievalServiceBlockingStub retrievalSvc() {
        return prepare(RetrievalServiceGrpc.newBlockingStub(channel), null, true);
    }

    private RunbookServiceGrpc.RunbookServiceBlockingStub runbookSvc() {
        return prepare(RunbookServiceGrpc.newBlockingStub(channel), null, true);
    }

    private ProviderServiceGrpc.ProviderServiceBlockingStub providerSvc() {
        return prepare(ProviderServiceGrpc.newBlockingStub(channel), null, true);
    }

    private SessionServiceGrpc.SessionServiceBlockingStub sessionSvc(boolean deadline) {
        return prepare(SessionServiceGrpc.newBlockingStub(channel), null, deadline);
    }

    private AdminServiceGrpc.AdminServiceBlockingStub adminSvc() {
        return prepare(AdminServiceGrpc.newBlockingStub(channel), null, true);
    }

    // -- commands -----------------------------------------------------------

    private String idemOrNew(String idem) {
        return idem != null ? idem : UUID.randomUUID().toString();
    }

    @Override
    public String createVersion(String parentVersionId, JsonNode metadata, String idem) {
        String key = idemOrNew(idem);
        var req = Command.CreateVersionRequest.newBuilder()
                .setParentVersionId(orEmpty(parentVersionId))
                .setMetadataJson(metadata == null ? "" : metadata.toString())
                .build();
        return command(() -> commands(key).createVersion(req)).getVersionId();
    }

    private static Command.ProposeClaimRequest claimPb(
            String versionId, Ledger.ClaimInput c, Long expectedHead) {
        if (c.confidence() != null && c.confidence() == 0.0) {
            throw new InvalidInputException(
                    "confidence = 0.0 cannot be represented on the gRPC wire (proto3 uses 0.0"
                            + " for 'absent'); omit it, or use the REST transport");
        }
        var b = Command.ProposeClaimRequest.newBuilder()
                .setVersionId(versionId)
                .setClaimType(claimTypePb(c.claimType()))
                .setSubject(c.subject())
                .setKey(c.key())
                .setValue(c.value())
                .setScopePath(orEmpty(c.scopePath()))
                .setProvenance(provenancePb(c.provenance()))
                .setSupersedesId(orEmpty(c.supersedesId()))
                .setEntityId(orEmpty(c.entityId()))
                .setEvidenceJson(c.evidence() == null ? "" : c.evidence().toString())
                .setConfidence(c.confidence() == null ? 0.0 : c.confidence())
                .setShapeRef(orEmpty(c.shapeRef()));
        if (c.origin() != null) {
            b.setOrigin(originPb(c.origin()));
        }
        if (expectedHead != null) {
            b.setExpectedHead(expectedHead); // proto3 optional: 0 carries presence
        }
        return b.build();
    }

    @Override
    public Ledger.ClaimOutcome proposeClaim(
            String versionId, Ledger.ClaimInput claim, Long expectedHead, String idem) {
        String key = idemOrNew(idem);
        var req = claimPb(versionId, claim, expectedHead);
        var resp = command(() -> commands(key).proposeClaim(req));
        if (!resp.hasClaim()) {
            throw new UnexpectedServerException("ProposeClaimResponse without claim", null);
        }
        return new Ledger.ClaimOutcome(
                claim(resp.getClaim()), findings(resp.getFindingsList()), resp.getHeadSeq());
    }

    @Override
    public Ledger.EventsOutcome appendEvents(String versionId, List<Ledger.ClaimInput> claims,
            String candidateText, Long expectedHead, String idem) {
        String key = idemOrNew(idem);
        var b = Command.AppendEventsRequest.newBuilder()
                .setVersionId(versionId)
                .setCandidateText(orEmpty(candidateText));
        for (Ledger.ClaimInput c : claims) {
            b.addClaims(claimPb(versionId, c, null));
        }
        if (expectedHead != null) {
            b.setExpectedHead(expectedHead);
        }
        var req = b.build();
        var resp = command(() -> commands(key).appendEvents(req));
        return new Ledger.EventsOutcome(
                resp.getClaimsList().stream().map(GrpcTransport::claim).toList(),
                findings(resp.getFindingsList()), resp.getHeadSeq());
    }

    @Override
    public Memory.Promise openPromise(String versionId, Params.PromiseInput p, String idem) {
        String key = idemOrNew(idem);
        var req = Command.OpenPromiseRequest.newBuilder()
                .setVersionId(versionId)
                .setKey(p.key())
                .setKind(p.kind())
                .setDescription(p.description())
                .setOriginScope(orEmpty(p.originScope()))
                .setDueScope(orEmpty(p.dueScope()))
                .build();
        var resp = command(() -> commands(key).openPromise(req));
        if (!resp.hasPromise()) {
            throw new UnexpectedServerException("OpenPromiseResponse without promise", null);
        }
        return promise(resp.getPromise());
    }

    @Override
    public boolean fulfillPromise(String versionId, String key, String idem) {
        String k = idemOrNew(idem);
        var req = Command.FulfillPromiseRequest.newBuilder()
                .setVersionId(versionId)
                .setKey(key)
                .build();
        return command(() -> commands(k).fulfillPromise(req)).getFulfilled();
    }

    @Override
    public Memory.Anchor lockAnchor(String versionId, Params.AnchorInput a, String idem) {
        String key = idemOrNew(idem);
        var req = Command.LockAnchorRequest.newBuilder()
                .setVersionId(versionId)
                .setSubject(a.subject())
                .setKey(a.key())
                .setValue(a.value())
                .setScopePath(orEmpty(a.scopePath()))
                .setEvidenceJson(a.evidence() == null ? "" : a.evidence().toString())
                .build();
        var resp = command(() -> commands(key).lockAnchor(req));
        if (!resp.hasAnchor()) {
            throw new UnexpectedServerException("LockAnchorResponse without anchor", null);
        }
        return anchor(resp.getAnchor());
    }

    @Override
    public void recordCounts(String versionId, String key, String scopePath, long count,
            Long budget, String idem) {
        rejectZero("budget", budget);
        String k = idemOrNew(idem);
        var req = Command.RecordCountsRequest.newBuilder()
                .setVersionId(versionId)
                .setKey(key)
                .setScopePath(scopePath)
                .setCount(count)
                .setBudget(budget == null ? 0 : budget)
                .build();
        command(() -> commands(k).recordCounts(req));
    }

    @Override
    public void upsertDigest(Memory.Digest d) {
        // gRPC UpsertDigest is a command RPC: idempotency-key metadata is
        // required (unlike the REST PUT, which is exempt by design).
        String key = UUID.randomUUID().toString();
        var req = Command.UpsertDigestRequest.newBuilder()
                .setDigest(Digest.newBuilder()
                        .setVersionId(d.versionId())
                        .setTier(d.tier())
                        .setScopePath(d.scopePath())
                        .setContent(d.content())
                        .setContentHash(d.contentHash())
                        .setBuiltFromSeq(d.builtFromSeq()))
                .build();
        command(() -> commands(key).upsertDigest(req));
    }

    // -- query --------------------------------------------------------------

    @Override
    public long head(String versionId) {
        var req = Query.GetHeadRequest.newBuilder().setVersionId(versionId).build();
        return read(() -> queries().getHead(req)).getHeadSeq();
    }

    @Override
    public Ledger.ClaimLookup getClaim(String claimId) {
        var req = Query.GetClaimRequest.newBuilder().setClaimId(claimId).build();
        var resp = read(() -> queries().getClaim(req));
        if (!resp.hasClaim()) {
            throw new UnexpectedServerException("GetClaimResponse without claim", null);
        }
        return new Ledger.ClaimLookup(
                claim(resp.getClaim()), resp.getSuperseded(), optStr(resp.getSupersededBy()));
    }

    @Override
    public Ledger.FactsPage facts(String versionId, Params.FactsQuery q) {
        rejectZero("as_of_seq", q.asOfSeq());
        rejectZero("limit", q.limit());
        var b = Query.SliceFactsRequest.newBuilder()
                .setVersionId(versionId)
                .setScopePrefix(orEmpty(q.scopePrefix()))
                .setAsOfSeq(q.asOfSeq() == null ? 0 : q.asOfSeq())
                .setLimit(q.limit() == null ? 0 : q.limit());
        if (q.statuses() != null) {
            for (String s : q.statuses()) {
                b.addStatuses(switch (s) {
                    case "accepted" -> Common.ClaimStatus.CLAIM_STATUS_ACCEPTED;
                    case "disputed" -> Common.ClaimStatus.CLAIM_STATUS_DISPUTED;
                    default -> throw new InvalidInputException(
                            "status must be accepted|disputed, got '" + s + "'");
                });
            }
        }
        var req = b.build();
        var resp = read(() -> queries().sliceFacts(req));
        var slice = resp.getSlice();
        return new Ledger.FactsPage(
                slice.getFactsList().stream().map(GrpcTransport::claim).toList(),
                slice.getAsOfSeq(), slice.getHeadSeq());
    }

    @Override
    public List<String> lineage(String versionId) {
        var req = Query.GetLineageRequest.newBuilder().setVersionId(versionId).build();
        return read(() -> queries().getLineage(req)).getLineage().getVersionIdsList();
    }

    @Override
    public List<Memory.Anchor> anchors(String versionId, Long asOfSeq) {
        rejectZero("as_of_seq", asOfSeq);
        var req = Query.ListAnchorsRequest.newBuilder()
                .setVersionId(versionId)
                .setAsOfSeq(asOfSeq == null ? 0 : asOfSeq)
                .build();
        return read(() -> queries().listAnchors(req)).getAnchorsList().stream()
                .map(GrpcTransport::anchor).toList();
    }

    @Override
    public List<Memory.Promise> promises(String versionId, Long asOfSeq, String status) {
        rejectZero("as_of_seq", asOfSeq);
        Wire.checkPromiseStatus(status);
        var req = Query.ListPromisesRequest.newBuilder()
                .setVersionId(versionId)
                .setStatus(orEmpty(status))
                .setAsOfSeq(asOfSeq == null ? 0 : asOfSeq)
                .build();
        return read(() -> queries().listPromises(req)).getPromisesList().stream()
                .map(GrpcTransport::promise).toList();
    }

    @Override
    public List<Memory.Counter> counters(String versionId, Long asOfSeq) {
        rejectZero("as_of_seq", asOfSeq);
        var req = Query.CounterTotalsRequest.newBuilder()
                .setVersionId(versionId)
                .setAsOfSeq(asOfSeq == null ? 0 : asOfSeq)
                .build();
        return read(() -> queries().counterTotals(req)).getCountersList().stream()
                .map(c -> new Memory.Counter(
                        c.getKey(), c.getTotal(), c.getBudget() == 0 ? null : c.getBudget()))
                .toList();
    }

    @Override
    public List<Memory.Digest> digests(String versionId) {
        var req = Query.ListDigestsRequest.newBuilder().setVersionId(versionId).build();
        return read(() -> queries().listDigests(req)).getDigestsList().stream()
                .map(d -> new Memory.Digest(d.getVersionId(), d.getTier(), d.getScopePath(),
                        d.getContent(), d.getContentHash(), d.getBuiltFromSeq()))
                .toList();
    }

    @Override
    public List<Ledger.StoredFinding> findings(String versionId, Params.FindingsQuery q) {
        throw unsupported("findings", "GET /v1/versions/{id}/findings");
    }

    // -- sealed evidence: REST-only in v1 -----------------------------------

    @Override
    public JsonNode evidence(String evidenceId) {
        throw unsupported("the sealed evidence plane", "GET /v1/evidence/{id}");
    }

    @Override
    public Evidence.EvidenceRows evidenceRows(String evidenceId, Params.EvidenceRowsQuery q) {
        throw unsupported("the sealed evidence plane", "GET /v1/evidence/{id}/rows");
    }

    @Override
    public Memory.ComposedContext composeContext(String versionId, Params.ContextQuery q) {
        rejectZero("as_of_seq", q.asOfSeq());
        rejectZero("fact_limit", q.factLimit());
        rejectZero("budget_tokens", q.budgetTokens());
        var req = Query.ComposeContextRequest.newBuilder()
                .setVersionId(versionId)
                .setScope(orEmpty(q.scope()))
                .setBudgetTokens(q.budgetTokens() == null ? 0 : q.budgetTokens())
                .setFactLimit(q.factLimit() == null ? 0 : q.factLimit())
                .setAsOfSeq(q.asOfSeq() == null ? 0 : q.asOfSeq())
                .build();
        ComposedContext ctx = read(() -> queries().composeContext(req)).getContext();
        return new Memory.ComposedContext(
                ctx.getSectionsList().stream()
                        .map(s -> new Memory.Section(s.getTitle(), s.getBody())).toList(),
                ctx.getText(), ctx.getEstimatedTokens(), ctx.getContentHash(), ctx.getAsOfSeq());
    }

    // -- ingest -------------------------------------------------------------

    @Override
    public Ingesting.PutSourceResult putSource(Params.ChunkSource data, Params.SourceMeta meta) {
        // Uploads are idempotent by content address, so transient failures
        // retry — the ChunkSource factory serves a FRESH stream per attempt.
        int attempt = 0;
        while (true) {
            attempt++;
            try {
                return putSourceOnce(data, meta);
            } catch (MunariumException e) {
                if (e.isTransient() && attempt <= options.readRetries()) {
                    Wire.sleepBackoff(attempt);
                    continue;
                }
                throw e;
            }
        }
    }

    private Ingesting.PutSourceResult putSourceOnce(Params.ChunkSource data, Params.SourceMeta meta) {
        var async = prepare(IngestServiceGrpc.newStub(channel), null, false);
        var latch = new CountDownLatch(1);
        var result = new AtomicReference<Ingest.PutSourceResponse>();
        var error = new AtomicReference<Throwable>();
        StreamObserver<Ingest.PutSourceRequest> req = async.putSource(
                new StreamObserver<>() {
                    @Override
                    public void onNext(Ingest.PutSourceResponse value) {
                        result.set(value);
                    }

                    @Override
                    public void onError(Throwable t) {
                        error.set(t);
                        latch.countDown();
                    }

                    @Override
                    public void onCompleted() {
                        latch.countDown();
                    }
                });
        try {
            req.onNext(Ingest.PutSourceRequest.newBuilder()
                    .setHeader(Ingest.SourceHeader.newBuilder()
                            .setDeclaredSha256(orEmpty(meta.declaredSha256()))
                            .setMediaType(orEmpty(meta.mediaType()))
                            .setFilename(orEmpty(meta.filename()))
                            .setShapeRef(orEmpty(meta.shapeRef())))
                    .build());
            try (InputStream in = data.open()) {
                byte[] buf = new byte[CHUNK_BYTES];
                int n;
                while ((n = in.read(buf)) >= 0) {
                    if (n > 0) {
                        req.onNext(Ingest.PutSourceRequest.newBuilder()
                                .setChunk(ByteString.copyFrom(buf, 0, n))
                                .build());
                    }
                }
            }
            req.onCompleted();
        } catch (IOException | RuntimeException e) {
            req.onError(e);
            // A send failure often MEANS the server already rejected the
            // stream (onNext throws once the call is cancelled) — the typed
            // status is sitting in the response observer. Prefer it over a
            // generic transient wrapper the retry loop would futilely
            // replay against the same refusal.
            try {
                if (latch.await(10, TimeUnit.SECONDS) && error.get() != null) {
                    if (error.get() instanceof StatusRuntimeException sre) {
                        throw decode(sre);
                    }
                }
            } catch (InterruptedException ie) {
                Thread.currentThread().interrupt();
            }
            throw new MunariumTransportException("chunk source failed: " + e, false);
        }
        try {
            latch.await();
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            throw new MunariumTransportException("interrupted", true);
        }
        if (error.get() != null) {
            if (error.get() instanceof StatusRuntimeException sre) {
                throw decode(sre);
            }
            throw new MunariumTransportException(error.get().toString(), true);
        }
        var resp = result.get();
        if (resp == null) {
            throw new UnexpectedServerException("PutSource completed without a response", null);
        }
        return new Ingesting.PutSourceResult(
                resp.getSourceId(), resp.getContentHash(), resp.getBytesLen(),
                resp.getAlreadyExisted());
    }

    @Override
    public Ingesting.RecordIngestResult recordIngest(
            String versionId, String contentHash, String shapeRef) {
        var req = Ingest.RecordIngestRequest.newBuilder()
                .setVersionId(versionId)
                .setContentHash(contentHash)
                .setShapeRef(orEmpty(shapeRef))
                .build();
        var resp = once(() -> ingestSvc().recordIngest(req));
        return new Ingesting.RecordIngestResult(resp.getEventId(), resp.getSeq());
    }

    @Override
    public Ingesting.IngestResult ingest(Ingesting.IngestFile file) {
        // Single-file parity with REST POST /v1/ingest, which returns a
        // typed 400 for an undecodable body: a local decode failure is an
        // EXCEPTION here, not a per-item result (per-item outcomes are the
        // BATCH contract). A server-side per-item error surfaces as an
        // unexpected-server exception carrying the text — the gRPC wire has
        // no slug for it (documented parity gap).
        Ingesting.IngestResult result = ingestBatch(List.of(file)).get(0);
        if (result.error() != null) {
            if (result.sourceId() == null && result.error().startsWith("content_base64")) {
                throw new InvalidInputException(result.error());
            }
            throw new UnexpectedServerException("ingest failed: " + result.error(), null);
        }
        return result;
    }

    /**
     * The per-item contract holds ACROSS transports: a file whose base64
     * cannot decode becomes its own error result (never sent), the valid
     * remainder ships, and results splice back in input order.
     */
    @Override
    public List<Ingesting.IngestResult> ingestBatch(List<Ingesting.IngestFile> files) {
        Wire.checkChunkSize("batch", files.size());
        for (Ingesting.IngestFile f : files) {
            if (f.collections() != null && f.collections().isEmpty()) {
                // REST `[]` = bind to NO collection; the proto3 empty repeated
                // field = absent = matcher auto-bind. A sentinel case, like 0.
                throw new InvalidInputException("collections = [] cannot be represented on"
                        + " the gRPC wire (proto3 empty = auto-bind); omit it, or use the"
                        + " REST transport");
            }
        }
        List<Ingesting.IngestResult> localErrors = new ArrayList<>();
        List<Boolean> sentSlot = new ArrayList<>();
        var reqB = Ingest.IngestFilesRequest.newBuilder();
        for (Ingesting.IngestFile f : files) {
            byte[] content;
            try {
                // The REST server TRIMS the base64 body before decoding, so a
                // trailing newline (the `base64` CLI's output) must succeed on
                // both transports.
                content = Base64.getDecoder().decode(
                        (f.contentBase64() == null ? "" : f.contentBase64())
                                .replaceAll("\\s+", ""));
            } catch (IllegalArgumentException e) {
                localErrors.add(new Ingesting.IngestResult(
                        f.filename(), null, null, false, List.of(),
                        "content_base64 is not valid base64: " + e.getMessage()));
                sentSlot.add(false);
                continue;
            }
            var fb = Ingest.IngestFile.newBuilder()
                    .setFilename(f.filename())
                    .setMediaType(orEmpty(f.mediaType()))
                    .setContent(ByteString.copyFrom(content))
                    .setSha256(orEmpty(f.sha256()));
            if (f.collections() != null) {
                fb.addAllCollections(f.collections());
            }
            reqB.addFiles(fb);
            sentSlot.add(true);
        }
        List<Ingesting.IngestResult> serverResults = new ArrayList<>();
        if (reqB.getFilesCount() > 0) {
            var req = reqB.build();
            // Content-addressed and per-item idempotent, but a batch can
            // partially apply — send once, like the REST file plane.
            // DEADLINE-EXEMPT like the REST file/bulk sends: a 500-file body
            // runs to the 256 MiB ceiling.
            var resp = once(() -> prepare(IngestServiceGrpc.newBlockingStub(channel), null, false)
                    .ingestFiles(req));
            for (Ingest.IngestResult r : resp.getResultsList()) {
                serverResults.add(new Ingesting.IngestResult(
                        r.getFilename(), optStr(r.getSourceId()), optStr(r.getSha256()),
                        r.getExisted(), r.getBoundToList(), optStr(r.getError())));
            }
        }
        int sentCount = (int) sentSlot.stream().filter(b -> b).count();
        if (serverResults.size() != sentCount) {
            // A surplus is as wrong as a shortfall: results splice back by
            // POSITION, so any count mismatch would mis-pair files silently.
            throw new UnexpectedServerException("IngestFilesResponse carried "
                    + serverResults.size() + " results for " + sentCount + " files sent", null);
        }
        List<Ingesting.IngestResult> out = new ArrayList<>();
        int localIdx = 0;
        int serverIdx = 0;
        for (boolean sent : sentSlot) {
            if (sent) {
                out.add(serverResults.get(serverIdx++));
            } else {
                out.add(localErrors.get(localIdx++));
            }
        }
        return out;
    }

    private static UnsupportedTransportException bulkUnsupported() {
        return new UnsupportedTransportException(
                "bulk upload sessions have no gRPC RPCs today — use the REST client"
                        + " (POST /v1/ingest/bulk …), or stream single sources via PutSource");
    }

    @Override
    public Ingesting.BulkOpenResult bulkOpen(List<Ingesting.BulkManifestEntry> files, String label) {
        throw bulkUnsupported();
    }

    @Override
    public Ingesting.BulkChunkResult bulkChunk(String bulkId, List<Ingesting.IngestFile> files) {
        throw bulkUnsupported();
    }

    @Override
    public Ingesting.BulkStatus bulkStatus(String bulkId, boolean includeNeeded) {
        throw bulkUnsupported();
    }

    @Override
    public Ingesting.BulkCompleteResult bulkComplete(String bulkId) {
        throw bulkUnsupported();
    }

    @Override
    public Ingesting.SourceInfo getSource(String sourceId) {
        throw unsupported("source metadata", "GET /v1/sources/{source_id}");
    }

    // -- retrieval ----------------------------------------------------------

    @Override
    public Retrieval.SearchResult search(Params.SearchQuery q) {
        rejectZero("top_k", q.topK());
        var req = mmp.v1.Retrieval.HybridSearchRequest.newBuilder()
                .setQuery(orEmpty(q.query()))
                .setShapeRef(orEmpty(q.shapeRef()))
                .setTopK(q.topK() == null ? 0 : q.topK())
                .setFilterJson(q.filter() == null ? "" : q.filter().toString())
                .setIndexVersion(orEmpty(q.indexVersion()))
                .build();
        // Search is a read: same retry class as the query plane.
        var resp = read(() -> retrievalSvc().hybridSearch(req));
        if (!resp.hasEnvelope()) {
            throw new UnexpectedServerException(
                    "HybridSearchResponse without ProvenanceEnvelope", null);
        }
        return new Retrieval.SearchResult(
                resp.getHitsList().stream()
                        .map(h -> new Retrieval.SearchHit(h.getChunkId(), h.getSourceId(),
                                h.getSourcePath(), h.getSourceContentHash(), h.getText(),
                                h.getScore(),
                                h.getLexicalRank() == 0.0 ? null : (int) h.getLexicalRank(),
                                h.getVectorRank() == 0.0 ? null : (int) h.getVectorRank(),
                                jsonOpt(h.getMetadataJson())))
                        .toList(),
                envelope(resp.getEnvelope()));
    }

    @Override
    public Retrieval.IndexStatus indexStatus(String shapeRef) {
        var req = mmp.v1.Retrieval.GetIndexVersionRequest.newBuilder()
                .setShapeRef(shapeRef)
                .build();
        var resp = read(() -> retrievalSvc().getIndexVersion(req));
        return new Retrieval.IndexStatus(resp.getIndexVersion(), shapeRef,
                resp.getEventWatermark(), resp.getActive(), jsonOpt(resp.getManifestJson()));
    }

    @Override
    public Retrieval.IndexStatus buildIndex(String shapeRef, String versionId) {
        throw unsupported("index builds", "POST /v1/indexes/{shape_ref}/build");
    }

    @Override
    public Retrieval.CollectionInfo createCollection(Params.CollectionSpec spec) {
        var b = mmp.v1.Retrieval.CreateCollectionRequest.newBuilder()
                .setName(spec.name())
                .setShapeRef(spec.shapeRef())
                .setAccessLevel(spec.accessLevel())
                .setDescription(orEmpty(spec.description()));
        if (spec.compartments() != null) {
            b.addAllCompartments(spec.compartments());
        }
        var req = b.build();
        // Create-or-update, but not replay-keyed: send once.
        return collection(once(() -> retrievalSvc().createCollection(req)));
    }

    @Override
    public List<Retrieval.CollectionInfo> listCollections() {
        var req = mmp.v1.Retrieval.ListCollectionsRequest.getDefaultInstance();
        return read(() -> retrievalSvc().listCollections(req)).getCollectionsList().stream()
                .map(GrpcTransport::collection).toList();
    }

    @Override
    public Retrieval.CollectionInfo getCollection(String id) {
        var req = mmp.v1.Retrieval.GetCollectionRequest.newBuilder().setId(id).build();
        return collection(read(() -> retrievalSvc().getCollection(req)));
    }

    // -- runbooks -----------------------------------------------------------

    @Override
    public Runbooks.ApplyShapeResult applyShape(String yaml, String versionId) {
        var req = Runbook.ApplyShapeRequest.newBuilder()
                .setYaml(yaml)
                .setVersionId(orEmpty(versionId))
                .build();
        var resp = once(() -> runbookSvc().applyShape(req));
        // The wire doesn't carry the hash, but it is defined as sha256(yaml
        // bytes) — computed locally for REST parity.
        return new Runbooks.ApplyShapeResult(resp.getShapeRef(), sha256Hex(yaml),
                optStr(resp.getEventId()));
    }

    private static String sha256Hex(String s) {
        try {
            var md = java.security.MessageDigest.getInstance("SHA-256");
            byte[] digest = md.digest(s.getBytes(java.nio.charset.StandardCharsets.UTF_8));
            StringBuilder sb = new StringBuilder(digest.length * 2);
            for (byte x : digest) {
                sb.append(Character.forDigit((x >> 4) & 0xf, 16))
                        .append(Character.forDigit(x & 0xf, 16));
            }
            return sb.toString();
        } catch (java.security.NoSuchAlgorithmException e) {
            throw new IllegalStateException("SHA-256 unavailable", e);
        }
    }

    @Override
    public String applyRunbook(String yaml) {
        var req = Runbook.ApplyRunbookRequest.newBuilder().setYaml(yaml).build();
        return once(() -> runbookSvc().applyRunbook(req)).getRunbookRef();
    }

    @Override
    public Runbooks.RunbookRun runRunbook(String name, String versionId) {
        // Params ride as JSON — through the mapper, never concatenation
        // (a versionId containing a quote must not forge extra members).
        String params = "";
        if (versionId != null) {
            var node = Json.MAPPER.createObjectNode();
            node.put("version_id", versionId);
            params = node.toString();
        }
        var req = Runbook.RunRunbookRequest.newBuilder()
                .setRunbookRef(name)
                .setParamsJson(params)
                .build();
        var resp = once(() -> runbookSvc().runRunbook(req));
        // State rides the response since the C1 additive proto field; fall
        // back to GetRun against older servers that predate it.
        String state = resp.getState().isEmpty() ? getRun(resp.getRunId()).state() : resp.getState();
        return new Runbooks.RunbookRun(resp.getRunId(), state);
    }

    @Override
    public Runbooks.RunStatus getRun(String runId) {
        var req = Runbook.GetRunRequest.newBuilder().setRunId(runId).build();
        var resp = read(() -> runbookSvc().getRun(req));
        return new Runbooks.RunStatus(resp.getRunId(), resp.getRunbookRef(), resp.getState(),
                optStr(resp.getVersionId()),
                resp.getStepsList().stream()
                        .map(s -> new Runbooks.RunbookStep(s.getOrdinal(), s.getName(),
                                s.getState(), jsonOpt(s.getDetailJson())))
                        .toList());
    }

    @Override
    public Runbooks.RunbookRun approveStep(String runId, int ordinal) {
        var req = Runbook.ApproveStepRequest.newBuilder()
                .setRunId(runId)
                .setStepOrdinal(ordinal)
                .build();
        var resp = once(() -> runbookSvc().approveStep(req));
        String state = resp.getState().isEmpty() ? getRun(runId).state() : resp.getState();
        return new Runbooks.RunbookRun(runId, state);
    }

    private static Runbooks.RunbookCollection runbookCollection(Runbook.RunbookCollectionInfo c) {
        return new Runbooks.RunbookCollection(c.getName(), optStr(c.getCollectionId()),
                c.getShapeRef(), c.getAccessLevel(), c.getCompartmentsList(),
                optStr(c.getActiveIndex()), c.getSourceCount());
    }

    @Override
    public List<Runbooks.RunbookSummary> list(boolean includeRemoved) {
        var req = Runbook.ListRunbooksRequest.newBuilder()
                .setIncludeRemoved(includeRemoved)
                .build();
        return read(() -> runbookSvc().listRunbooks(req)).getRunbooksList().stream()
                .map(r -> new Runbooks.RunbookSummary(r.getRunbookRef(), r.getName(),
                        r.getVersion(), r.getStatus(), r.getMinAccessLevel(),
                        r.getCollectionsList().stream()
                                .map(GrpcTransport::runbookCollection).toList(),
                        r.getCreatedAt()))
                .toList();
    }

    @Override
    public Runbooks.RunbookInfo getInfo(String name) {
        var req = Runbook.GetRunbookInfoRequest.newBuilder().setName(name).build();
        var resp = read(() -> runbookSvc().getRunbookInfo(req));
        JsonNode retrieval = jsonOpt(resp.getRetrievalJson());
        return new Runbooks.RunbookInfo(resp.getRunbookRef(), resp.getName(), resp.getVersion(),
                resp.getStatus(),
                resp.getCollectionsList().stream().map(GrpcTransport::runbookCollection).toList(),
                resp.getVersionsList(), jsonOpt(resp.getModelsJson()),
                retrieval == null ? com.fasterxml.jackson.databind.node.NullNode.getInstance()
                        : retrieval,
                resp.getHasCompletion(), resp.getCreatedAt());
    }

    @Override
    public Runbooks.ValidateResult validate(String yaml, Params.ValidateOptions o) {
        var req = Runbook.ValidateRunbookRequest.newBuilder()
                .setYaml(yaml)
                .setSuggest(o.suggest())
                .setProvider(orEmpty(o.provider()))
                .setModel(orEmpty(o.model()))
                .setTier(orEmpty(o.tier()))
                .build();
        // With suggest=true this spends provider tokens — send once.
        var resp = once(() -> runbookSvc().validateRunbook(req));
        return new Runbooks.ValidateResult(resp.getValid(),
                resp.getFindingsList().stream()
                        .map(f -> new Runbooks.ValidationFinding(f.getSeverity(), f.getCode(),
                                f.getMessage(), f.getPath()))
                        .toList(),
                resp.getSuggestionsList().stream()
                        .map(s -> new Runbooks.Suggestion(s.getTitle(), s.getRationale(),
                                optStr(s.getPatchHint())))
                        .toList(),
                optStr(resp.getSuggestNote()));
    }

    @Override
    public Runbooks.RemovalRequest removeRequest(String name) {
        var req = Runbook.RequestRemovalRequest.newBuilder().setRunbookRef(name).build();
        var resp = once(() -> runbookSvc().requestRemoval(req));
        return new Runbooks.RemovalRequest(
                resp.getRunbookRef(), resp.getRemovalId(), resp.getExpiresAt());
    }

    @Override
    public Runbooks.RemovalConfirm removeConfirm(String name, String removalId) {
        var req = Runbook.ConfirmRemovalRequest.newBuilder()
                .setRunbookRef(name)
                .setRemovalId(removalId)
                .build();
        var resp = once(() -> runbookSvc().confirmRemoval(req));
        return new Runbooks.RemovalConfirm(resp.getRunbookRef(), resp.getStatus());
    }

    @Override
    public Runbooks.ChronologyRulesResult applyChronologyRules(String yaml) {
        throw unsupported("chronology rules", "POST /v1/chronology-rules");
    }

    @Override
    public String getChronologyRules(String name) {
        throw unsupported("chronology rules", "GET /v1/chronology-rules/{name}");
    }

    // -- providers ----------------------------------------------------------

    @Override
    public String applyConfig(String yaml) {
        var req = Provider.ApplyProviderConfigRequest.newBuilder().setYaml(yaml).build();
        return once(() -> providerSvc().applyProviderConfig(req)).getConfigName();
    }

    @Override
    public Providers.ProviderHealth health(String name) {
        var req = Provider.ProviderHealthRequest.newBuilder().setConfigName(name).build();
        var resp = read(() -> providerSvc().providerHealth(req));
        return new Providers.ProviderHealth(resp.getHealthy(), resp.getProvider(),
                resp.getEndpointFingerprint(), resp.getDetail());
    }

    @Override
    public Providers.HealthAiResult healthAi() {
        throw unsupported("healthai", "GET /healthai");
    }

    @Override
    public Providers.CompleteResult complete(String name, Params.CompleteOptions o) {
        if (o.temperature() != null && o.temperature() == 0.0) {
            throw new InvalidInputException(
                    "temperature = 0.0 cannot be represented on the gRPC wire (proto3 uses 0.0"
                            + " for 'absent'); omit it, or use the REST transport");
        }
        rejectZero("max_tokens", o.maxTokens());
        var req = Provider.CompleteRequest.newBuilder()
                .setConfigName(name)
                .setModel(orEmpty(o.model()))
                .setSystem(orEmpty(o.system()))
                .setPrompt(orEmpty(o.prompt()))
                .setMaxTokens(o.maxTokens() == null ? 0 : o.maxTokens())
                .setTemperature(o.temperature() == null ? 0.0 : o.temperature())
                .setVersionId(orEmpty(o.versionId()))
                .setProvider(orEmpty(o.provider()))
                .setTier(orEmpty(o.tier()))
                .build();
        var resp = once(() -> providerSvc().complete(req));
        return new Providers.CompleteResult(resp.getText(), resp.getStopReason(),
                resp.getInputTokens(), resp.getOutputTokens(), resp.getProvider(),
                resp.getModel(), optStr(resp.getInvocationEventId()));
    }

    @Override
    public Providers.EmbedResult embed(String name, Params.EmbedOptions o) {
        var req = Provider.EmbedRequest.newBuilder()
                .setConfigName(name)
                .setModel(orEmpty(o.model()))
                .addAllInputs(o.inputs())
                .setVersionId(orEmpty(o.versionId()))
                .setProvider(orEmpty(o.provider()))
                .build();
        var resp = once(() -> providerSvc().embed(req));
        return new Providers.EmbedResult(
                resp.getVectorsList().stream()
                        .map(v -> v.getValuesList().stream().map(Float::doubleValue).toList())
                        .toList(),
                resp.getDimensions(), resp.getCacheHit(), resp.getProvider(), resp.getModel(),
                optStr(resp.getInvocationEventId()));
    }

    @Override
    public Providers.ProviderList list() {
        throw unsupported("provider disclosure", "GET /v1/providers");
    }

    @Override
    public Providers.MaxTokensResponse maxTokens() {
        throw unsupported("max-tokens budgets", "GET /v1/max-tokens");
    }

    @Override
    public Providers.MaxTokensResponse replaceMaxTokens(Providers.MaxTokensBudgets budgets) {
        throw unsupported("max-tokens budgets", "POST /v1/max-tokens");
    }

    // -- sessions -----------------------------------------------------------

    @Override
    public SessionsApi.CreateSessionResult create(String runbookName) {
        var req = Session.CreateSessionRequest.newBuilder().setRunbookName(runbookName).build();
        // Opens server-side state — send once.
        var resp = once(() -> sessionSvc(true).createSession(req));
        return new SessionsApi.CreateSessionResult(
                resp.getSessionId(), resp.getRunbookRef(), resp.getPermittedCollectionsList());
    }

    @Override
    public SessionsApi.TurnResult turn(String sessionId, Params.TurnOptions o) {
        rejectZero("top_k", o.topK());
        var b = Session.TurnRequest.newBuilder()
                .setSessionId(sessionId)
                .setQuery(o.query())
                .setTopK(o.topK() == null ? 0 : o.topK())
                .setComplete(Boolean.TRUE.equals(o.complete()));
        if (o.modelOverride() != null) {
            b.setModelOverride(Session.SessionModelOverride.newBuilder()
                    .setProvider(orEmpty(o.modelOverride().provider()))
                    .setModel(orEmpty(o.modelOverride().model()))
                    .setTier(orEmpty(o.modelOverride().tier())));
        }
        // proto3 scalar: empty string IS "not set", which is exactly the
        // legacy document path — no presence wrapper needed.
        b.setResearchProfile(orEmpty(o.researchProfile()));
        var req = b.build();
        // Send-once, never auto-retried, and DEADLINE-EXEMPT like the REST
        // twin: aborting client-side does not stop the paid completion.
        var resp = once(() -> sessionSvc(false).turn(req));
        return turnResult(resp);
    }

    private static SessionsApi.TurnResult turnResult(Session.TurnResponse resp) {
        List<SessionsApi.CollectionEnvelope> envelopes = new ArrayList<>();
        for (Session.CollectionEnvelope e : resp.getEnvelopesList()) {
            if (!e.hasEnvelope()) {
                throw new UnexpectedServerException(
                        "CollectionEnvelope for '" + e.getCollection()
                                + "' without ProvenanceEnvelope", null);
            }
            envelopes.add(new SessionsApi.CollectionEnvelope(
                    e.getCollection(), envelope(e.getEnvelope())));
        }
        SessionsApi.TurnCompletion completion = null;
        if (resp.hasCompletion()) {
            var c = resp.getCompletion();
            SessionsApi.TurnVerification verification = null;
            if (c.hasVerification()) {
                var v = c.getVerification();
                verification = new SessionsApi.TurnVerification(v.getChecksList(),
                        v.getRetries(), v.getFirstPassViolationsList(), v.getViolationsList());
            }
            completion = new SessionsApi.TurnCompletion(c.getProvider(), c.getModel(),
                    c.getWasOverride(), c.getText(), c.getInputTokens(), c.getOutputTokens(),
                    verification);
        }
        // Present only when a research profile ran; a legacy turn leaves the
        // message unset and the record member null, matching REST.
        SessionsApi.EvidenceHierarchyDecision hierarchy =
                resp.hasHierarchy() ? hierarchy(resp.getHierarchy()) : null;
        return new SessionsApi.TurnResult(resp.getSessionId(), resp.getOrdinal(),
                resp.getCollectionsSearchedList(), resp.getSkippedList(),
                resp.getHitsList().stream()
                        .map(h -> new SessionsApi.TurnHit(h.getCollection(), h.getChunkId(),
                                h.getSourceId(), h.getSourcePath(), h.getSourceContentHash(),
                                h.getText(), h.getScore()))
                        .toList(),
                envelopes, completion, hierarchy);
    }

    private static SessionsApi.EvidenceHierarchyDecision hierarchy(
            Session.EvidenceHierarchyDecision d) {
        // optStr on the three genuinely-optional strings: proto3 cannot tell
        // "absent" from "empty", and REST sends them absent — reporting a
        // layer's refusal code as "" would invent a refusal that never
        // happened.
        return new SessionsApi.EvidenceHierarchyDecision(
                d.getProfile(),
                optStr(d.getIntentKind()),
                d.getIntentExplicit(),
                d.getLayersList().stream()
                        .map(l -> new SessionsApi.LayerOutcome(l.getLayer(), l.getRole(),
                                l.getRequirement(), l.getBlock(), optStr(l.getEvidenceId()),
                                l.getSupportsCompleteness(), optStr(l.getRefusalCode()),
                                l.getElapsedMs()))
                        .toList(),
                d.getCompletenessAvailable(),
                d.getDisclosedConflicts(),
                d.getConflictsPolicy());
    }

    @Override
    public SessionsApi.TurnResult turnStream(String sessionId, Params.TurnOptions o,
            Consumer<SessionsApi.TurnProgress> onProgress) {
        throw new UnsupportedTransportException(
                "streaming turns have no gRPC RPC today — use the REST client"
                        + " (POST /v1/sessions/{id}/turns/stream), or the unary turn here");
    }

    private static SessionsApi.Session session(Session.GetSessionResponse resp) {
        return new SessionsApi.Session(resp.getSessionId(), resp.getUid(), resp.getRunbookRef(),
                resp.getAccessLevel(), resp.getCompartmentsList(), resp.getState(),
                resp.getCreatedAt(),
                resp.getTurnsList().stream()
                        .map(t -> new SessionsApi.SessionTurn(t.getOrdinal(), t.getQuery(),
                                t.getCollectionsSearchedList(),
                                // Stored transcript rows ride as JSON strings
                                // — parse-or-null keeps a mangled row visible
                                // instead of failing the whole session read.
                                jsonOpt(t.getHitsJson()), jsonOpt(t.getEnvelopeJson()),
                                jsonOpt(t.getCompletionJson()), t.getCreatedAt()))
                        .toList());
    }

    @Override
    public SessionsApi.Session get(String sessionId) {
        var req = Session.GetSessionRequest.newBuilder().setSessionId(sessionId).build();
        return session(read(() -> sessionSvc(true).getSession(req)));
    }

    @Override
    public SessionsApi.Session close(String sessionId) {
        var req = Session.CloseSessionRequest.newBuilder().setSessionId(sessionId).build();
        // Idempotent by construction server-side, but still a write — sent
        // once, matching the REST transport.
        return session(once(() -> sessionSvc(true).closeSession(req)));
    }

    // -- access tokens (AdminService's served trio) --------------------------

    @Override
    public Tokens.TokenGrant mint(Tokens.IssueTokenRequest r) {
        rejectZero("ttl_secs", r.ttlSecs());
        if (r.runbookRefs() != null && r.runbookRefs().isEmpty()) {
            // REST `[]` = no runbook allowed; proto3 empty = any runbook.
            throw new InvalidInputException("runbook_refs = [] cannot be represented on the"
                    + " gRPC wire (proto3 empty = any runbook); omit it, or use the REST"
                    + " transport");
        }
        var b = Admin.IssueAccessTokenRequest.newBuilder()
                .setUid(r.uid())
                .setAccessLevel(r.accessLevel())
                .setTtlSecs(r.ttlSecs() == null ? 0 : r.ttlSecs());
        if (r.compartments() != null) {
            b.addAllCompartments(r.compartments());
        }
        if (r.scopes() != null) {
            b.addAllScopes(r.scopes());
        }
        if (r.runbookRefs() != null) {
            b.addAllRunbookRefs(r.runbookRefs());
        }
        var req = b.build();
        // Minting twice issues two live tokens — send once.
        var resp = once(() -> adminSvc().issueAccessToken(req));
        return new Tokens.TokenGrant(resp.getToken(), resp.getJti(), resp.getExpiresAt());
    }

    @Override
    public List<Tokens.TokenInfo> list(Params.TokenListQuery q) {
        var req = Admin.ListAccessTokensRequest.newBuilder()
                .setUid(orEmpty(q.uid()))
                // proto3 bool: false = "all" — identical to the REST default,
                // so null and FALSE land on the same wire value by design.
                .setActive(Boolean.TRUE.equals(q.active()))
                .build();
        return read(() -> adminSvc().listAccessTokens(req)).getTokensList().stream()
                .map(t -> new Tokens.TokenInfo(t.getJti(), t.getUid(), t.getAccessLevel(),
                        t.getCompartmentsList(), t.getScopesList(),
                        t.getRunbookRefsList().isEmpty() ? null : t.getRunbookRefsList(),
                        t.getIssuedBy(), t.getIssuedAt(), t.getExpiresAt(),
                        optStr(t.getRevokedAt())))
                .toList();
    }

    @Override
    public Tokens.RevokeResult revoke(String jti) {
        var req = Admin.RevokeAccessTokenRequest.newBuilder().setJti(jti).build();
        var resp = once(() -> adminSvc().revokeAccessToken(req));
        return new Tokens.RevokeResult(
                resp.getJti(), resp.getRevoked(), resp.getRevocationCheckEnabled());
    }

    // -- reports / authoring: REST-only surfaces, honestly typed -------------

    private static UnsupportedTransportException reportsUnsupported() {
        return new UnsupportedTransportException(
                "reports have no gRPC RPCs today (AdminService.Usage is declared but"
                        + " UNIMPLEMENTED) — use the REST client (GET /v1/reports/…)");
    }

    @Override
    public Reports.UsageReport usage(Params.UsageQuery q) {
        throw reportsUnsupported();
    }

    @Override
    public Reports.AuditPage audit(Params.AuditQuery q) {
        throw reportsUnsupported();
    }

    @Override
    public Reports.CostReport cost(String from, String to) {
        throw reportsUnsupported();
    }

    @Override
    public Reports.TimeseriesReport timeseries(String window, String plane) {
        throw reportsUnsupported();
    }

    @Override
    public Reports.EndpointsReport endpoints(String window, Long limit) {
        throw reportsUnsupported();
    }

    @Override
    public Reports.RunbookReport runbooks(String window) {
        throw reportsUnsupported();
    }

    @Override
    public Reports.SessionsReport sessions(String window) {
        throw reportsUnsupported();
    }

    @Override
    public Reports.EvidenceReport evidenceReport(String window) {
        throw reportsUnsupported();
    }

    @Override
    public Reports.MatrixReport matrix() {
        throw reportsUnsupported();
    }

    private static UnsupportedTransportException authoringUnsupported() {
        return new UnsupportedTransportException(
                "guided authoring has no gRPC RPCs — use the REST client (/v1/authoring/…)");
    }

    @Override
    public Authoring.PatternPage listPatterns() {
        throw authoringUnsupported();
    }

    @Override
    public Authoring.PatternDetail getPattern(String id) {
        throw authoringUnsupported();
    }

    @Override
    public Authoring.Draft createDraft(Authoring.CreateDraftRequest request) {
        throw authoringUnsupported();
    }

    @Override
    public Authoring.DraftPage listDrafts() {
        throw authoringUnsupported();
    }

    @Override
    public Authoring.Draft getDraft(String draftId) {
        throw authoringUnsupported();
    }

    @Override
    public Authoring.DraftDelete deleteDraft(String draftId) {
        throw authoringUnsupported();
    }

    @Override
    public Authoring.Draft putAnswers(String draftId, JsonNode answers, boolean materialize) {
        throw authoringUnsupported();
    }

    @Override
    public Authoring.DraftValidation validate(String draftId) {
        throw authoringUnsupported();
    }

    @Override
    public Authoring.AssistResult assist(String draftId, Authoring.AssistRequest request) {
        throw authoringUnsupported();
    }

    @Override
    public Authoring.ExportBundle export(String draftId) {
        throw authoringUnsupported();
    }

    @Override
    public Authoring.ApplyDraftResult apply(String draftId) {
        throw authoringUnsupported();
    }
}
