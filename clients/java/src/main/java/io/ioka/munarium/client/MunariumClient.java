// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client;

import io.ioka.munarium.client.errors.HeadConflictException;
import io.ioka.munarium.client.grpc.GrpcTransport;
import io.ioka.munarium.client.internal.Wire;
import io.ioka.munarium.client.model.Ledger;
import io.ioka.munarium.client.model.Meta;
import io.ioka.munarium.client.planes.Planes;
import io.ioka.munarium.client.rest.RestTransport;
import java.util.function.LongFunction;

/**
 * Official Java client for munarium-server — the SYNCHRONOUS facade: ten plane
 * namespaces over one connection + auth config, transport chosen at
 * construction ({@link #rest} / {@link #grpc}).
 * {@link AsyncMunariumClient} is the {@code CompletableFuture} twin.
 *
 * <pre>{@code
 * try (var client = MunariumClient.rest(
 *         MunariumClientOptions.of("http://127.0.0.1:8080").withToken("devtoken").withUid("user-1"))) {
 *     String v = client.commands.createVersion();
 *     var outcome = client.commands.proposeClaim(
 *             v, Ledger.ClaimInput.fact("hero", "eyes", "green"), null, null);
 * }
 * }</pre>
 *
 * <p>The invariants this client encodes: (1) disputed is NOT an error — a
 * gate-blocked claim returns success with {@code isDisputed()} + findings;
 * (2) head conflicts are normal — {@link #proposeClaimWithRetry} re-reads,
 * rebuilds, retries with a fresh idempotency key per attempt; (3) one
 * {@code asOfSeq} pin bounds every query; (4) every retrieval answer
 * carries a ProvenanceEnvelope; (5) append-only — no update/delete methods,
 * corrections name {@code supersedesId} explicitly; (6) idempotency keys
 * auto-generate per command and are caller-overridable; (7) commands are
 * never auto-retried once the request may have been delivered; (8)
 * governance enums decode fail-closed ON THE gRPC WIRE — an unknown tag
 * can never read as "the gates passed" (REST carries the registry's
 * strings verbatim); (9) typed errors keyed on the
 * problem-slug registry — no English message text is ever parsed.
 */
public final class MunariumClient implements AutoCloseable {
    public final Planes.CommandsPlane commands;
    public final Planes.QueryPlane query;
    public final Planes.IngestPlane ingest;
    public final Planes.RetrievalPlane retrieval;
    public final Planes.RunbooksPlane runbooks;
    public final Planes.ProvidersPlane providers;
    public final Planes.SessionsPlane sessions;
    public final Planes.AccessTokensPlane tokens;
    public final Planes.ReportsPlane reports;
    public final Planes.AuthoringPlane authoring;

    /**
     * Sealed evidence READS (REST-only). Resolve an
     * {@code [evidence/<id>#<row>]} citation to what an answer was computed
     * from. Sealing is not here on purpose — see
     * {@link Planes.EvidencePlane}.
     */
    public final Planes.EvidencePlane evidence;

    private final Transport transport;

    private MunariumClient(Transport transport) {
        // One typed Transport = every plane: a transport missing one is a
        // COMPILE error, and the ten fields are just plane-scoped views.
        this.commands = transport;
        this.query = transport;
        this.ingest = transport;
        this.retrieval = transport;
        this.runbooks = transport;
        this.providers = transport;
        this.sessions = transport;
        this.tokens = transport;
        this.reports = transport;
        this.authoring = transport;
        this.evidence = transport;
        this.transport = transport;
    }

    /** REST transport ({@code :8080} in the demo posture; {@code :443} behind gateways). */
    public static MunariumClient rest(MunariumClientOptions options) {
        return new MunariumClient(new RestTransport(options));
    }

    /** Direct gRPC transport ({@code :50051}, or {@code :443} via the gateway). */
    public static MunariumClient grpc(MunariumClientOptions options) {
        return new MunariumClient(new GrpcTransport(options));
    }

    /**
     * {@code GET /version} — the served name + version (REST only; the gRPC
     * transport throws the same typed unsupported error every other
     * REST-only surface uses). Compare against
     * {@link Munarium#TARGET_SERVER_VERSION} to catch a stale deploy early.
     */
    public Meta.ServerVersion serverVersion() {
        return transport.serverVersion();
    }

    /**
     * The head-conflict write loop (invariant #2): read head → build the
     * claim via {@code build.apply(head)} → propose with
     * {@code expectedHead = head} and a FRESH idempotency key → on conflict
     * back off (jittered), rebuild against the actual head, retry. Never
     * retries other errors. {@code maxAttempts} includes the first try.
     */
    public Ledger.ClaimOutcome proposeClaimWithRetry(
            String versionId, LongFunction<Ledger.ClaimInput> build, int maxAttempts) {
        long head = query.head(versionId);
        int attempt = 0;
        while (true) {
            attempt++;
            try {
                return commands.proposeClaim(versionId, build.apply(head), head, null);
            } catch (HeadConflictException e) {
                if (attempt >= maxAttempts) {
                    throw e;
                }
                Wire.sleepBackoff(attempt);
                // actual == 0 means the transport carried no structured
                // seqs — re-read the head instead of trusting the sentinel.
                head = e.actual() > 0 ? e.actual() : query.head(versionId);
            }
        }
    }

    public Ledger.ClaimOutcome proposeClaimWithRetry(
            String versionId, LongFunction<Ledger.ClaimInput> build) {
        return proposeClaimWithRetry(versionId, build, 3);
    }

    @Override
    public void close() {
        transport.close();
    }
}
