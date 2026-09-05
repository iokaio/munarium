// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.matrix;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.function.Supplier;

/**
 * The {@code CompletableFuture} twin of {@link MatrixClient}.
 *
 * <p>One implementation, zero drift: each call offloads the corresponding
 * blocking method to a VIRTUAL thread. On Java 21 blocking is the scalable
 * primitive, so a second hand-written non-blocking path would buy nothing and
 * would be free to disagree with the first about what a refusal means.
 *
 * <p>The surface is <b>method-for-method</b> the same, and a test asserts it.
 * A method that exists on one twin and not the other is a trap for a caller
 * porting between them: it compiles on the day they write it and fails when
 * they reach the one call the other twin never grew.
 *
 * <p>Futures fail with {@link MatrixException} wrapped in a
 * {@code CompletionException}, per the {@code CompletableFuture} convention.
 *
 * <p>Like the synchronous client, this speaks REST only. There is no gRPC
 * transport, because Matrix's gRPC plane serves {@code Execute} alone and that
 * call is service-to-service.
 */
public final class AsyncMatrixClient implements AutoCloseable {

    private final MatrixClient delegate;
    private final ExecutorService executor;

    public AsyncMatrixClient(MatrixClientOptions options) {
        this(new MatrixClient(options));
    }

    public AsyncMatrixClient(MatrixClient delegate) {
        this.delegate = delegate;
        this.executor = Executors.newVirtualThreadPerTaskExecutor();
    }

    public static AsyncMatrixClient of(String endpoint) {
        return new AsyncMatrixClient(MatrixClientOptions.of(endpoint));
    }

    public static AsyncMatrixClient of(String endpoint, String token) {
        return new AsyncMatrixClient(MatrixClientOptions.of(endpoint).withToken(token));
    }

    /**
     * The blocking client underneath, sharing this one's connection and auth.
     *
     * <p>Named {@code blocking()} rather than {@code sync()} because
     * {@code sync(String)} is a Matrix OPERATION on this surface — materialize
     * a source — and two unrelated meanings on one class is exactly how a
     * reader ends up materializing a collection while reaching for an
     * accessor.
     */
    public MatrixClient blocking() {
        return delegate;
    }

    @Override
    public void close() {
        executor.close();
        delegate.close();
    }

    // -- meta -----------------------------------------------------------------

    public CompletableFuture<Version> version() {
        return async(delegate::version);
    }

    public CompletableFuture<Boolean> healthz() {
        return async(delegate::healthz);
    }

    public CompletableFuture<HealthData> healthdata() {
        return async(delegate::healthdata);
    }

    // -- registry -------------------------------------------------------------

    public CompletableFuture<ApplyOutcome> apply(String yaml) {
        return async(() -> delegate.apply(yaml));
    }

    public CompletableFuture<Validation> validate(String yaml) {
        return async(() -> delegate.validate(yaml));
    }

    public CompletableFuture<List<AssetSummary>> listAssets(String kind) {
        return async(() -> delegate.listAssets(kind));
    }

    public CompletableFuture<List<AssetSummary>> listAssets(String kind, boolean allVersions) {
        return async(() -> delegate.listAssets(kind, allVersions));
    }

    public CompletableFuture<String> getYaml(String kind, String name) {
        return async(() -> delegate.getYaml(kind, name));
    }

    // -- sources --------------------------------------------------------------

    public CompletableFuture<JsonNode> introspect(String source) {
        return async(() -> delegate.introspect(source));
    }

    public CompletableFuture<Probe> probe(String source) {
        return async(() -> delegate.probe(source));
    }

    public CompletableFuture<JobAccepted> sync(String source) {
        return async(() -> delegate.sync(source));
    }

    // -- contracts and views --------------------------------------------------

    public CompletableFuture<VerifyOutcome> verify(String contract) {
        return async(() -> delegate.verify(contract));
    }

    public CompletableFuture<VerifyOutcome> verifyView(String view) {
        return async(() -> delegate.verifyView(view));
    }

    // -- reconcile ------------------------------------------------------------

    public CompletableFuture<JobAccepted> reconcile(String mapping) {
        return async(() -> delegate.reconcile(mapping));
    }

    public CompletableFuture<PromotionStatus> promotionStatus(String mapping) {
        return async(() -> delegate.promotionStatus(mapping));
    }

    public CompletableFuture<GateHistory> gateHistory(String mapping) {
        return async(() -> delegate.gateHistory(mapping));
    }

    public CompletableFuture<GateHistory> gateHistory(String mapping, int limit) {
        return async(() -> delegate.gateHistory(mapping, limit));
    }

    public CompletableFuture<PromotionStatus> promote(
            String mapping, String decisionId, String actor, String reason) {
        return async(() -> delegate.promote(mapping, decisionId, actor, reason));
    }

    public CompletableFuture<PromotionStatus> promote(String mapping, String decisionId, String actor) {
        return async(() -> delegate.promote(mapping, decisionId, actor));
    }

    public CompletableFuture<PromotionStatus> demote(String mapping, String decisionId) {
        return async(() -> delegate.demote(mapping, decisionId));
    }

    public CompletableFuture<RollbackOutcome> rollback(String mapping, String decisionId) {
        return async(() -> delegate.rollback(mapping, decisionId));
    }

    // -- audit ----------------------------------------------------------------

    public CompletableFuture<List<JsonNode>> journal() {
        return async(delegate::journal);
    }

    public CompletableFuture<List<JsonNode>> journal(int limit) {
        return async(() -> delegate.journal(limit));
    }

    // -------------------------------------------------------------------------

    private <T> CompletableFuture<T> async(Supplier<T> work) {
        return CompletableFuture.supplyAsync(work, executor);
    }
}
