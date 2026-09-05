// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.conformance;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.ioka.munarium.client.MunariumClient;
import io.ioka.munarium.client.errors.HeadConflictException;
import io.ioka.munarium.client.model.Ledger;
import io.ioka.munarium.client.model.Memory;
import io.ioka.munarium.client.planes.Params;
import java.util.List;
import org.junit.jupiter.api.Disabled;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.params.ParameterizedTest;
import org.junit.jupiter.params.provider.ValueSource;

/**
 * The server's 7 MMP conformance scenarios, ported to the black-box API
 * surface (same names as {@code server/conformance/src/lib.rs} so CI output
 * is comparable across languages), parameterized over both transports.
 *
 * <p>Two scenarios differ from the Rust suite by design (the same two
 * documented deviations as the Python and .NET ports):
 * {@code composer.budget-degradation} and {@code digests.rebuilt-under-pin}
 * assert via the server's ComposeContext (the Rust suite composes
 * client-side through munarium-core, which Java does not link);
 * {@code gates.chronology-certain-only} is a pure-kernel check with no API
 * surface — SCENARIOS.md marks it kernel-only; no client port carries it,
 * disabled here.
 */
class ScenariosTest {

    private static Ledger.ClaimInput fact(String subject, String key, String value) {
        return Ledger.ClaimInput.fact(subject, key, value);
    }

    @ParameterizedTest(name = "ledger.origin-round-trips [{0}]")
    @ValueSource(strings = {"rest", "grpc"})
    void ledgerOriginRoundTrips(String transport) {
        // S-4.1: a connector claim's origin survives the round trip on both
        // transports; a claim proposed without one reads back without one.
        try (MunariumClient c = Env.forTransport(transport)) {
            String v = c.commands.createVersion();
            var origin = new Ledger.ClaimOrigin(
                    "connector", "crm", "captable-holdings@1", "holder_id=43",
                    "lsn/0/1A2B", "2026-08-28T09:15:00Z", "ev-batch-0001");
            var out = c.commands.proposeClaim(
                    v, fact("shareholder.43", "shares", "90500").withOrigin(origin), null, null);
            assertEquals(origin, out.claim().origin(), "the write echo must carry origin");

            var read = c.query.getClaim(out.claim().id());
            assertEquals("holder_id=43", read.claim().origin().rowKey(),
                    "a fresh read must return the origin, not just the write echo");

            var plain = c.commands.proposeClaim(v, fact("shareholder.43", "class", "A"), null, null);
            assertEquals(null, plain.claim().origin(), "no origin in, no origin out");
        }
    }

    @ParameterizedTest(name = "ledger.append-head-conflict [{0}]")
    @ValueSource(strings = {"rest", "grpc"})
    void ledgerAppendHeadConflict(String transport) {
        try (MunariumClient c = Env.forTransport(transport)) {
            String v = c.commands.createVersion();
            var out = c.commands.proposeClaim(v, fact("hero", "eyes", "green"), 0L, null);
            assertEquals(1, out.claim().seq(), "first append must get seq 1");

            assertThrows(HeadConflictException.class,
                    () -> c.commands.proposeClaim(v, fact("hero", "home", "harbor"), 0L, null));
            assertEquals(1, c.query.head(v), "failed append must not advance head");
        }
    }

    @ParameterizedTest(name = "ledger.supersession-pin [{0}]")
    @ValueSource(strings = {"rest", "grpc"})
    void ledgerSupersessionPin(String transport) {
        try (MunariumClient c = Env.forTransport(transport)) {
            String v1 = c.commands.createVersion();
            var original = c.commands.proposeClaim(v1, fact("hero", "eyes", "green"), null, null);
            c.commands.proposeClaim(v1, fact("hero", "home", "harbor"), null, null);

            // correction in a CHILD version supersedes across the lineage
            String v2 = c.commands.createVersion(v1, null, null);
            c.commands.proposeClaim(v2,
                    Ledger.ClaimInput.correction("hero", "eyes", "blue", original.claim().id()),
                    null, null);

            var head = c.query.facts(v2, Params.FactsQuery.all()).facts().stream()
                    .filter(f -> f.key().equals("eyes")).toList();
            assertEquals(1, head.size());
            assertEquals("blue", head.get(0).value(), "head must read the correction");

            var pinned = c.query.facts(v2, Params.FactsQuery.atSeq(2)).facts().stream()
                    .filter(f -> f.key().equals("eyes")).toList();
            assertEquals(1, pinned.size());
            assertEquals("green", pinned.get(0).value(),
                    "claim superseded after the pin must read as current at the pin");
        }
    }

    @ParameterizedTest(name = "pins.one-pin-bounds-all-stores [{0}]")
    @ValueSource(strings = {"rest", "grpc"})
    void pinsOnePinBoundsAllStores(String transport) {
        try (MunariumClient c = Env.forTransport(transport)) {
            String v = c.commands.createVersion();
            c.commands.proposeClaim(v, fact("hero", "eyes", "green"), null, null); // seq 1
            c.commands.openPromise(v,
                    new Params.PromiseInput("reveal", "setup", "open the letter", "ch1", "ch3"),
                    null); // seq 2 (registration advances the clock)
            c.commands.lockAnchor(v,
                    new Params.AnchorInput("hero", "eyes", "green", "ch1", null), null); // seq 3
            c.commands.recordCounts(v, "flashback", "ch1", 1, 2L, null); // seq 4
            c.commands.proposeClaim(v, fact("hero", "home", "harbor"), null, null); // seq 5
            c.commands.fulfillPromise(v, "reveal", null); // fulfilled_seq 6

            // pin at 1: only claim 1 exists — nothing later may leak back
            assertTrue(c.query.anchors(v, 1L).isEmpty(),
                    "anchor stamped at seq 3 must be invisible at pin 1");
            assertTrue(c.query.counters(v, 1L).isEmpty(),
                    "counter stamped at seq 4 must be invisible at pin 1");
            assertTrue(c.query.promises(v, 1L, null).isEmpty(),
                    "promise registered at seq 2 must be invisible at pin 1");

            // pin at 2: promise registered and OPEN; anchor + counter ahead
            assertTrue(c.query.anchors(v, 2L).isEmpty());
            assertTrue(c.query.counters(v, 2L).isEmpty());
            List<Memory.Promise> promises = c.query.promises(v, 2L, null);
            assertEquals(1, promises.size(), "promise registered at seq 2 visible at pin 2");
            assertEquals("open", promises.get(0).status(),
                    "post-pin fulfillment must read back OPEN");

            // head: everything visible, promise fulfilled
            assertFalse(c.query.anchors(v, null).isEmpty(), "anchor at head");
            assertEquals("fulfilled", c.query.promises(v, null, null).get(0).status());
        }
    }

    @ParameterizedTest(name = "gates.block-records-disputed [{0}]")
    @ValueSource(strings = {"rest", "grpc"})
    void gatesBlockRecordsDisputed(String transport) {
        try (MunariumClient c = Env.forTransport(transport)) {
            String v = c.commands.createVersion();
            c.commands.proposeClaim(v, fact("hero", "eyes", "green"), null, null);

            // the command path IS the governance path: the conflicting claim
            // comes back SUCCESS with status disputed + the gate finding.
            var out = c.commands.proposeClaim(v,
                    fact("hero", "eyes", "blue").withScopePath("ch2"), null, null);
            assertTrue(out.isDisputed(), "conflicting plain claim must be recorded disputed");
            assertTrue(out.findings().stream().anyMatch(f ->
                            f.ruleId().equals("gate.ledger-conflict") && f.severity().equals("block")),
                    "expected gate.ledger-conflict block, got " + out.findings());

            var accepted = c.query.facts(v, Params.FactsQuery.all()).facts().stream()
                    .filter(f -> f.key().equals("eyes")).toList();
            assertEquals(1, accepted.size());
            assertEquals("green", accepted.get(0).value(), "canon must be unchanged");

            var disputed = c.query.facts(v,
                    new Params.FactsQuery(null, null, List.of("disputed"), null)).facts();
            assertTrue(disputed.stream().anyMatch(f -> f.value().equals("blue")),
                    "the blocked claim must be recorded disputed, not dropped");
        }
    }

    @ParameterizedTest(name = "composer.budget-degradation [{0}]")
    @ValueSource(strings = {"rest", "grpc"})
    void composerBudgetDegradation(String transport) {
        try (MunariumClient c = Env.forTransport(transport)) {
            String v = c.commands.createVersion();
            for (int i = 1; i <= 20; i++) {
                c.commands.proposeClaim(v,
                        fact("hero", "k" + i, "value-" + i + " with prose attached")
                                .withScopePath(i <= 10 ? "book.ch1" : "book.ch2"),
                        null, null);
            }
            var full = c.query.composeContext(v,
                    new Params.ContextQuery("book.ch1", null, null, null));
            long budget = full.estimatedTokens() - 20;
            var degraded = c.query.composeContext(v,
                    new Params.ContextQuery("book.ch1", budget, null, null));
            assertTrue(degraded.estimatedTokens() <= budget, "budget must hold");
            long kept = degraded.sections().stream()
                    .filter(s -> s.title().equals("Accepted facts"))
                    .findFirst()
                    .map(s -> s.body().lines().count())
                    .orElse(0L);
            assertEquals(20, kept,
                    "digests must degrade BEFORE facts trim (facts kept: " + kept + ")");
        }
    }

    @ParameterizedTest(name = "digests.rebuilt-under-pin [{0}]")
    @ValueSource(strings = {"rest", "grpc"})
    void digestsRebuiltUnderPin(String transport) {
        try (MunariumClient c = Env.forTransport(transport)) {
            String v = c.commands.createVersion();
            c.commands.proposeClaim(v,
                    fact("hero", "eyes", "green").withScopePath("ch1"), null, null); // seq 1
            c.commands.proposeClaim(v,
                    fact("hero", "home", "harbor").withScopePath("ch1"), null, null); // seq 2

            // store a HEAD-shaped digest, then pin before seq 2: the stored
            // rung (mentioning "home") must never be served under the pin.
            c.commands.upsertDigest(new Memory.Digest(
                    v, 0, "ch1", "[ch1] hero eyes green; hero home harbor", "head-shaped", 2));
            var pinned = c.query.composeContext(v, new Params.ContextQuery(null, null, null, 1L));
            assertFalse(pinned.text().contains("home"),
                    "stored head digests must never be served under a pin");
        }
    }

    @Test
    @Disabled("gates.chronology-certain-only: pure-kernel scenario (client-side"
            + " check_chronology over declarative rules; no API surface) — SCENARIOS.md"
            + " marks it kernel-only and no client port carries it")
    void gatesChronologyCertainOnly() {
        // intentionally empty — see @Disabled reason
    }

    /** The async facade, proven end-to-end on both transports (one path per
     * transport is enough: every async method is the same one-line virtual
     * thread offload over the sync plane — zero drift by construction). */
    @ParameterizedTest(name = "async.facade-round-trip [{0}]")
    @ValueSource(strings = {"rest", "grpc"})
    void asyncFacadeRoundTrip(String transport) throws Exception {
        org.junit.jupiter.api.Assumptions.assumeTrue(
                Env.REST_URL != null && Env.GRPC_URL != null, "live URLs unset");
        String token = Env.TOKEN != null ? Env.TOKEN : "devtoken";
        var options = io.ioka.munarium.client.MunariumClientOptions
                .of("grpc".equals(transport) ? Env.GRPC_URL : Env.REST_URL)
                .withToken(token)
                .withUid("conformance-async");
        try (var c = "grpc".equals(transport)
                ? io.ioka.munarium.client.AsyncMunariumClient.grpc(options)
                : io.ioka.munarium.client.AsyncMunariumClient.rest(options)) {
            String v = c.commands.createVersion().get();
            var outcome = c.commands
                    .proposeClaim(v, fact("hero", "eyes", "green"), null, null)
                    .thenCompose(o -> c.query.facts(v, Params.FactsQuery.all()))
                    .get();
            assertEquals(1, outcome.facts().size());
            assertEquals("green", outcome.facts().get(0).value());
        }
    }
}
