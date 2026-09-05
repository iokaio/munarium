// SPDX-License-Identifier: Apache-2.0
package io.ioka.munarium.client.conformance;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.ioka.munarium.client.MunariumClient;
import io.ioka.munarium.client.errors.ForbiddenException;
import io.ioka.munarium.client.errors.InvalidInputException;
import io.ioka.munarium.client.errors.MunariumException;
import io.ioka.munarium.client.errors.NotFoundException;
import io.ioka.munarium.client.errors.UnsupportedTransportException;
import io.ioka.munarium.client.model.Authoring;
import io.ioka.munarium.client.model.Ingesting;
import io.ioka.munarium.client.model.Json;
import io.ioka.munarium.client.model.SessionsApi;
import io.ioka.munarium.client.model.Tokens;
import io.ioka.munarium.client.planes.Params;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;
import org.junit.jupiter.api.AfterAll;
import org.junit.jupiter.api.BeforeAll;
import org.junit.jupiter.api.MethodOrderer.OrderAnnotation;
import org.junit.jupiter.api.Order;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.TestInstance;
import org.junit.jupiter.api.TestMethodOrder;

/**
 * The platform surface, proven through the TYPED planes — a native
 * port of the Rust client's platform smokes (same scenario order and
 * assertions; see {@code clients/rust/munarium-client-conformance/src/platform_smoke.rs}).
 * Requires a mgmt static token on the SAME tenant as MUNARIUM_TOKEN and
 * MUNARIUM_TOKEN_SECRET server-side; skips cleanly when MUNARIUM_MGMT_TOKEN is
 * unset. Zero provider keys — nothing here completes.
 *
 * <p>Ordered on purpose: the application scenario mints the token the SSE
 * scenario uses — the same ordering dependency the server's own suite has.
 * Re-runnable against a shared dev tenant BY DESIGN (nonce'd content,
 * nonce'd doomed removal version).
 */
@TestInstance(TestInstance.Lifecycle.PER_CLASS)
@TestMethodOrder(OrderAnnotation.class)
class PlatformSmokesTest {
    private MunariumClient ops;
    private MunariumClient mgr;
    private String bobToken;

    private static final String SHAPE_YAML =
            "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: { name: entdocs, version: 1 }\n"
                    + "spec:\n  fact:\n    schema: { type: object }\n";

    private static String runbookYaml(long version) {
        return """
                apiVersion: munarium.ioka.io/v1
                kind: Runbook
                metadata: { name: ent-support, version: %d }
                spec:
                  collections:
                    - name: ent-public
                      shape: entdocs@1
                      accessLevel: 0
                      sources: { filenamePrefix: "public/" }
                    - name: ent-secret
                      shape: entdocs@1
                      accessLevel: 2
                      compartments: [eng]
                      sources: { filenamePrefix: "eng/" }
                  retrieval: { topK: 5 }
                  models:
                    default: { provider: default, tier: fast }
                    allowOverrides: [default]
                  completion:
                    promptTemplate: "Answer from context only.\\n{context}\\n\\nQ: {query}"
                  steps:
                    - resolveSources: {}
                    - buildIndex: {}
                    - verify: {}
                    - cutover: { approval: required }
                    - retireOld: { keep_versions: 2 }
                """
                .formatted(version);
    }

    private static String sha256(String s) {
        try {
            var md = MessageDigest.getInstance("SHA-256");
            var digest = md.digest(s.getBytes(StandardCharsets.UTF_8));
            var sb = new StringBuilder();
            for (byte b : digest) {
                sb.append(Character.forDigit((b >> 4) & 0xf, 16))
                        .append(Character.forDigit(b & 0xf, 16));
            }
            return sb.toString();
        } catch (Exception e) {
            throw new AssertionError(e);
        }
    }

    @BeforeAll
    void setUp() {
        String mgmt = Env.requireMgmt();
        String rw = Env.TOKEN != null ? Env.TOKEN : "devtoken";
        ops = Env.rest(rw, "ops");
        mgr = Env.rest(mgmt, "mgr");
    }

    @AfterAll
    void tearDown() {
        if (ops != null) {
            ops.close();
        }
        if (mgr != null) {
            mgr.close();
        }
    }

    private Tokens.TokenGrant mint(String uid, int level, List<String> compartments,
            List<String> scopes) {
        return mgr.tokens.mint(new Tokens.IssueTokenRequest(
                uid, level, compartments, scopes, null, null));
    }

    private MunariumClient rest(String token, String uid) {
        return Env.rest(token, uid);
    }

    @Test
    @Order(1)
    void uidContract() {
        String rw = Env.TOKEN != null ? Env.TOKEN : "devtoken";
        try (MunariumClient noUid = MunariumClient.rest(
                io.ioka.munarium.client.MunariumClientOptions.of(Env.REST_URL).withToken(rw))) {
            var e = assertThrows(InvalidInputException.class, () -> noUid.runbooks.list(false));
            assertTrue(e.getMessage().contains("uid"),
                    "uid-required detail should name the uid: " + e.getMessage());
        }
        var grant = mint("uid-alice", 0, List.of(), List.of("query"));
        try (MunariumClient mallory = rest(grant.token(), "mallory")) {
            assertThrows(ForbiddenException.class, () -> mallory.runbooks.list(false),
                    "uid mismatch must be typed Forbidden");
        }
    }

    @Test
    @Order(2)
    void rolePartition() {
        assertThrows(ForbiddenException.class,
                () -> ops.tokens.mint(Tokens.IssueTokenRequest.of("x", 0, List.of("query"))),
                "rw minting must be Forbidden");
        assertThrows(ForbiddenException.class, () -> mgr.commands.createVersion(),
                "mgmt ledger write must be Forbidden");
    }

    @Test
    @Order(3)
    void applicationAndCompartments() {
        ops.runbooks.applyShape(SHAPE_YAML, null);

        // Validation first: clean passes, topK: 0 invalidates.
        assertTrue(ops.runbooks.validate(runbookYaml(1), Params.ValidateOptions.deterministic())
                .valid(), "clean runbook must validate");
        assertFalse(ops.runbooks.validate(runbookYaml(1).replace("topK: 5", "topK: 0"),
                Params.ValidateOptions.deterministic()).valid(), "topK: 0 must invalidate");

        ops.runbooks.applyRunbook(runbookYaml(1));

        // Ingest via the file plane under the ingest scope; matchers auto-bind.
        var loaderGrant = mint("loader", 2, List.of("eng"), List.of("ingest"));
        try (MunariumClient loader = rest(loaderGrant.token(), "loader")) {
            var results = loader.ingest.ingestBatch(List.of(
                    Ingesting.IngestFile.ofText("public/handbook.md", "text/markdown",
                            "The public handbook grants twenty vacation days."),
                    Ingesting.IngestFile.ofText("eng/launch.md", "text/markdown",
                            "Secret launch window: vacation blackout in Q4.")));
            assertEquals(2, results.size());
            assertEquals(List.of("ent-public"), results.get(0).boundTo(),
                    "matcher auto-bind: " + results);
            assertEquals(List.of("ent-secret"), results.get(1).boundTo());
        }

        // A level-0 ingest token must NOT write into ent-secret.
        var lowGrant = mint("lowloader", 0, List.of(), List.of("ingest"));
        try (MunariumClient low = rest(lowGrant.token(), "lowloader")) {
            assertThrows(ForbiddenException.class, () -> low.ingest.ingest(
                    Ingesting.IngestFile.ofText("sneak.md", "text/markdown", "nope")
                            .withCollections(List.of("ent-secret"))));
        }

        // Run with two per-collection approval passes.
        var run = ops.runbooks.runRunbook("ent-support", null);
        assertEquals("awaiting_approval", run.state(), "run must pause");
        for (int pass = 0; pass < 2; pass++) {
            var status = ops.runbooks.getRun(run.runId());
            var awaiting = status.steps().stream()
                    .filter(s -> s.state().equals("awaiting_approval"))
                    .findFirst()
                    .orElseThrow(() -> new AssertionError("no step awaiting approval: " + status));
            ops.runbooks.approveStep(run.runId(), awaiting.ordinal());
        }
        assertEquals("done", ops.runbooks.getRun(run.runId()).state(), "run must finish");

        // List + info expose per-collection access requirements.
        var entry = ops.runbooks.list(false).stream()
                .filter(b -> b.runbookRef().equals("ent-support@1"))
                .findFirst()
                .orElseThrow(() -> new AssertionError("ent-support@1 missing from list"));
        var levels = entry.collections().stream().map(c -> c.accessLevel()).toList();
        assertTrue(levels.contains(0) && levels.contains(2), "levels 0 and 2: " + levels);
        var info = ops.runbooks.getInfo("ent-support");
        assertTrue(info.collections().size() == 2 && info.hasCompletion());

        // Two clearances, one runbook: disjoint result sets for one query.
        var aliceGrant = mint("comp-alice", 0, List.of(), List.of("query"));
        var bobGrant = mint("comp-bob", 2, List.of("eng"), List.of("query"));
        try (MunariumClient alice = rest(aliceGrant.token(), "comp-alice");
                MunariumClient bob = rest(bobGrant.token(), "comp-bob")) {
            var sessionA = alice.sessions.create("ent-support");
            assertEquals(List.of("ent-public"), sessionA.permittedCollections(),
                    "alice must see only ent-public");
            var sessionB = bob.sessions.create("ent-support");
            assertEquals(2, sessionB.permittedCollections().size(), "bob sees both");

            var turnA = alice.sessions.turn(sessionA.sessionId(), Params.TurnOptions.of("vacation"));
            assertFalse(turnA.hits().isEmpty());
            assertTrue(turnA.hits().stream().allMatch(h -> h.collection().equals("ent-public")),
                    "alice hits must be ent-public only");

            var turnB = bob.sessions.turn(sessionB.sessionId(), Params.TurnOptions.of("vacation"));
            assertTrue(turnB.hits().stream().anyMatch(h -> h.collection().equals("ent-secret")),
                    "bob's merged hits must include ent-secret");
            assertEquals(2, turnB.envelopes().size(), "one envelope per collection");

            // Multiturn continuity, transcript readback, cross-uid refusal.
            var turn2 = bob.sessions.turn(sessionB.sessionId(), Params.TurnOptions.of("blackout"));
            assertEquals(2, turn2.ordinal(), "follow-on turn must be ordinal 2");
            var readback = bob.sessions.get(sessionB.sessionId());
            assertTrue(readback.turns().size() == 2 && readback.state().equals("open"));
            assertThrows(ForbiddenException.class, () -> alice.sessions.turn(
                    sessionB.sessionId(), Params.TurnOptions.of("x")), "cross-uid turn");

            // Model-override policy refusal (BEFORE any provider spend).
            assertThrows(ForbiddenException.class, () -> bob.sessions.turn(
                    sessionB.sessionId(),
                    Params.TurnOptions.of("x")
                            .withCompletion(SessionsApi.ModelOverride.provider(
                                    "not-allowed-provider"))),
                    "disallowed override");

            // Scope enforcement: a query token cannot ingest.
            assertThrows(ForbiddenException.class, () -> bob.ingest.ingest(
                    Ingesting.IngestFile.ofText("x.md", "text/markdown", "x")), "scope-missing");
        }
        bobToken = bobGrant.token();
    }

    @Test
    @Order(4)
    void removalDoublePass() {
        // Nonce'd doomed version: removal is permanent, so a fixed number
        // would make this scenario single-use against a shared dev tenant.
        long doomedVersion = (System.currentTimeMillis() / 1000) % 2_000_000_000L;
        String doomed = "ent-support@" + doomedVersion;
        ops.runbooks.applyRunbook(runbookYaml(doomedVersion));

        // Single-pass confirm refused; wrong removal_id draws the SAME typed
        // refusal (accepting any error would let a 503 masquerade as the
        // double-pass guard working).
        assertThrows(InvalidInputException.class,
                () -> ops.runbooks.removeConfirm(doomed, "rm-guess"));
        var removal = ops.runbooks.removeRequest(doomed);
        assertFalse(removal.removalId().isEmpty());
        assertThrows(InvalidInputException.class,
                () -> ops.runbooks.removeConfirm(doomed, "rm-wrong"));
        assertEquals("removed",
                ops.runbooks.removeConfirm(doomed, removal.removalId()).status());

        // Removed exact ref: typed NotFound (410); bare name resolves live.
        var grant = mint("rm-user", 0, List.of(), List.of("query"));
        try (MunariumClient user = rest(grant.token(), "rm-user")) {
            assertThrows(NotFoundException.class, () -> user.sessions.create(doomed));
            var live = user.sessions.create("ent-support");
            assertTrue(live.runbookRef().startsWith("ent-support@")
                    && !live.runbookRef().equals(doomed), "bare name resolves to a live version");
        }

        // Hidden from the default list; visible with include_removed.
        assertTrue(ops.runbooks.list(false).stream()
                .noneMatch(b -> b.runbookRef().equals(doomed)));
        assertTrue(ops.runbooks.list(true).stream()
                .anyMatch(b -> b.runbookRef().equals(doomed)));
    }

    @Test
    @Order(5)
    void reportsAndRevoke() {
        assertThrows(ForbiddenException.class,
                () -> ops.reports.usage(Params.UsageQuery.byUid()), "rw on reports");

        var usage = mgr.reports.usage(Params.UsageQuery.byUid());
        var keys = usage.rows().stream().map(r -> r.key()).toList();
        assertTrue(keys.contains("comp-alice") && keys.contains("comp-bob"),
                "usage rows must include the session uids: " + keys);

        assertFalse(mgr.reports.audit(Params.AuditQuery.forUid("comp-bob")).entries().isEmpty(),
                "audit for comp-bob must be non-empty");

        // The dashboard-view reports answer too.
        assertEquals("24h", mgr.reports.timeseries("24h", null).window());
        assertFalse(mgr.reports.endpoints("24h", 5L).rows().isEmpty());
        assertNotNull(mgr.reports.runbooks("24h"));
        assertTrue(mgr.reports.sessions("24h").buckets().stream().anyMatch(b -> b.turns() > 0),
                "sessions report must show the turns this suite took");
        assertNotNull(mgr.reports.cost(null, null));

        // S-3.5: the evidence-hierarchy and Matrix operator views. This
        // suite takes only legacy turns, so the assertion is about the
        // report ANSWERING, not about hierarchy traffic existing.
        var evidence = mgr.reports.evidenceReport("24h");
        assertEquals("24h", evidence.window());
        assertNotNull(evidence.layers());
        assertNotNull(mgr.reports.matrix().dataViews());

        // Revoke: the deny-list row lands and the audit shows it.
        var grant = mint("revokee", 0, List.of(), List.of("query"));
        assertTrue(mgr.tokens.revoke(grant.jti()).revoked());
        var tokens = mgr.tokens.list(Params.TokenListQuery.forUid("revokee"));
        assertTrue(!tokens.isEmpty() && tokens.get(0).revokedAt() != null,
                "issuance audit must show revoked_at");
    }

    @Test
    @Order(6)
    void authoringLifecycle() {
        assertEquals(7, ops.authoring.listPatterns().patterns().size(), "the 7 §19 patterns");
        assertTrue(ops.authoring.getPattern("ask-the-corpus").runbookYaml()
                .contains("kind: Runbook"), "pattern detail carries the exemplar");

        var draft = ops.authoring.createDraft(
                Authoring.CreateDraftRequest.of("vendor-security", "ask-the-corpus"));
        assertFalse(draft.draftId().isEmpty());
        assertEquals("identity", draft.interview().get(0).id(), "interview starts at identity");

        assertTrue(ops.authoring.listDrafts().drafts().stream()
                .anyMatch(d -> d.draftId().equals(draft.draftId())));
        assertEquals("vendor-security", ops.authoring.getDraft(draft.draftId()).name());

        // A blank draft refuses to export (409 authoring-draft-invalid).
        assertThrows(InvalidInputException.class, () -> ops.authoring.export(draft.draftId()));

        var answers = """
                {"identity.description": "Vendor security reviews for procurement.",
                 "prefix.root": "vendors/",
                 "prefix.areas": [
                   {"path": "public/", "description": "published attestations"},
                   {"path": "contracts/", "description": "signed agreements"}],
                 "access.uniform_public": false,
                 "access.area_levels": {"public": 0, "contracts": 2},
                 "access.area_compartments": {"contracts": ["legal"]}}
                """;
        Authoring.Draft updated;
        try {
            updated = ops.authoring.putAnswers(
                    draft.draftId(), Json.MAPPER.readTree(answers), true);
        } catch (java.io.IOException e) {
            throw new AssertionError(e);
        }
        assertTrue(updated.validation() != null && updated.validation().valid(),
                "canonical answers must validate clean");
        assertEquals(2, updated.documents().size(), "one shape + one runbook");

        // Assist DEGRADES keyless: success + assistNote, documents intact.
        var assist = ops.authoring.assist(draft.draftId(), Authoring.AssistRequest.empty());
        assertNotNull(assist.assistNote(), "keyless assist must carry a degrade note");
        assertEquals(2, assist.documents().size());

        assertTrue(ops.authoring.validate(draft.draftId()).valid());

        // Export: verify the manifest CLIENT-side, exactly as mmctl does.
        var bundle = ops.authoring.export(draft.draftId());
        assertEquals("MunariumAuthoringBundle", bundle.kind());
        var buf = new StringBuilder();
        for (var e : bundle.files().entrySet()) { // BTreeMap server-side = sorted
            String actual = sha256(e.getValue());
            assertEquals(bundle.hashes().get(e.getKey()), actual,
                    "per-file hash mismatch for " + e.getKey());
            buf.append(e.getKey()).append('\0').append(actual).append('\n');
        }
        assertEquals(bundle.manifestHash(), sha256(buf.toString()), "manifest hash");
        assertTrue(bundle.applyOrder().get(0).startsWith("shapes/"), "shapes apply first");

        assertEquals(2, ops.authoring.apply(draft.draftId()).applied().size());
        assertEquals(2, ops.runbooks.getInfo("vendor-security").collections().size(),
                "applied runbook reaches its two collections");

        // Draft cleanup — the client surface's one DELETE.
        assertEquals("deleted", ops.authoring.deleteDraft(draft.draftId()).status());
    }

    @Test
    @Order(7)
    void bulkUploadLifecycle() {
        ops.runbooks.applyShape(
                "apiVersion: munarium.ioka.io/v1\nkind: Shape\nmetadata: { name: bulkdocs, version: 1 }\n"
                        + "spec:\n  fact:\n    schema: { type: object }\n",
                null);
        ops.runbooks.applyRunbook("""
                apiVersion: munarium.ioka.io/v1
                kind: Runbook
                metadata: { name: bulk-archive, version: 1 }
                spec:
                  collections:
                    - name: bulk-open-docs
                      shape: bulkdocs@1
                      accessLevel: 0
                      sources: { filenamePrefix: "bulkdocs/" }
                  retrieval: { topK: 5 }
                  steps:
                    - resolveSources: {}
                    - buildIndex: {}
                    - verify: {}
                    - cutover: { approval: required }
                    - retireOld: { keep_versions: 2 }
                """);

        var loaderGrant = mint("bulkloader", 0, List.of(), List.of("ingest"));
        try (MunariumClient loader = rest(loaderGrant.token(), "bulkloader")) {
            // Nonce'd contents: the zero-byte-re-run assertion needs fresh
            // bytes on a shared dev server.
            String n = Env.nonce();
            String a = "Bulk document alpha " + n + ": the treaty was signed.";
            String b = "Bulk document beta " + n + ": the harbor closed in March.";
            String c = "Bulk document gamma " + n + ": the assembly dissolved.";

            // CLIENT-side guard: an over-cap chunk never leaves the process.
            var oversized = new ArrayList<Ingesting.IngestFile>();
            for (int i = 0; i < 501; i++) {
                oversized.add(Ingesting.IngestFile.ofText("bulkdocs/f" + i + ".md",
                        "text/markdown", "x"));
            }
            var cap = assertThrows(InvalidInputException.class,
                    () -> loader.ingest.bulkChunk("blk-any", oversized));
            assertTrue(cap.getMessage().contains("500"), "cap error names the cap");

            // Manifest validation server-side: duplicates rejected whole.
            assertThrows(InvalidInputException.class, () -> loader.ingest.bulkOpen(
                    List.of(manifest("bulkdocs/a.md", a), manifest("bulkdocs/a.md", a)), null));

            // Open: fresh manifest, all three needed.
            var open = loader.ingest.bulkOpen(List.of(
                    manifest("bulkdocs/a.md", a), manifest("bulkdocs/b.md", b),
                    manifest("bulkdocs/c.md", c)), "java-conformance");
            assertTrue(open.total() == 3 && open.alreadyPresent() == 0
                    && open.needed().size() == 3, "fresh open needs all three: " + open);

            // Chunk 1: a good; b deliberately corrupt (per-file sha mismatch).
            var chunk1 = loader.ingest.bulkChunk(open.bulkId(), List.of(
                    file("bulkdocs/a.md", a), file("bulkdocs/b.md", "corrupted bytes")));
            assertEquals(2, chunk1.results().size());
            assertEquals(null, chunk1.results().get(0).error(), "a.md must store");
            assertTrue(chunk1.results().get(1).error() != null
                    && chunk1.results().get(1).error().contains("sha256 mismatch"),
                    "corrupt b.md must fail per-file: " + chunk1);
            assertTrue(chunk1.stored() == 1 && chunk1.failed() == 1 && chunk1.pending() == 1);

            // Early finalize: incomplete, naming what is owed.
            var early = loader.ingest.bulkComplete(open.bulkId());
            assertTrue(early.status().equals("incomplete") && early.missingCount() == 2);

            // Chunk 2 — wholesale replay: a again (idempotent), b fixed, c.
            var chunk2 = loader.ingest.bulkChunk(open.bulkId(), List.of(
                    file("bulkdocs/a.md", a), file("bulkdocs/b.md", b), file("bulkdocs/c.md", c)));
            assertEquals(3, chunk2.results().size());
            assertTrue(chunk2.results().get(0).existed(), "replayed a.md is an idempotent no-op");
            assertTrue(chunk2.pending() == 0 && chunk2.failed() == 0);

            // Finalize + status agree; stored count survives replay.
            assertEquals("completed", loader.ingest.bulkComplete(open.bulkId()).status());
            var status = loader.ingest.bulkStatus(open.bulkId(), true);
            assertTrue(status.status().equals("completed") && status.needed().isEmpty()
                    && status.stored() == 3, "status after complete: " + status);

            // Zero-byte re-run: same manifest, nothing owed.
            var rerun = loader.ingest.bulkOpen(List.of(
                    manifest("bulkdocs/a.md", a), manifest("bulkdocs/b.md", b),
                    manifest("bulkdocs/c.md", c)), null);
            assertTrue(rerun.alreadyPresent() == 3 && rerun.needed().isEmpty());
            assertEquals("completed", loader.ingest.bulkComplete(rerun.bulkId()).status());

            // Unknown session: typed NotFound.
            assertThrows(NotFoundException.class,
                    () -> loader.ingest.bulkStatus("blk-doesnotexist", false));

            // get_source is a CONTROL-plane read: a capability JWT draws the
            // typed 403; the rw static token reads the metadata back.
            String sourceId = chunk2.results().get(2).sourceId();
            assertNotNull(sourceId, "c.md must carry a source_id");
            assertThrows(ForbiddenException.class, () -> loader.ingest.getSource(sourceId));
            var sourceInfo = ops.ingest.getSource(sourceId);
            assertTrue(sourceInfo.filename().equals("bulkdocs/c.md")
                    && sourceInfo.contentHash().equals(sha256(c)),
                    "source metadata must match what was uploaded");
        }
    }

    private static Ingesting.BulkManifestEntry manifest(String name, String text) {
        return new Ingesting.BulkManifestEntry(name, sha256(text),
                text.getBytes(StandardCharsets.UTF_8).length, "text/markdown");
    }

    private static Ingesting.IngestFile file(String name, String text) {
        return Ingesting.IngestFile.ofText(name, "text/markdown", text);
    }

    @Test
    @Order(8)
    void routeCoverage() {
        // GET /version (unauthenticated meta).
        var version = ops.serverVersion();
        assertTrue(version.name().equals("munarium-server") && !version.version().isEmpty());

        // Collections trio (depends on entdocs@1 from the application scenario).
        String name = "cov-java-" + Env.nonce();
        var created = ops.retrieval.createCollection(new Params.CollectionSpec(
                name, "entdocs@1", 1, List.of("cov"), "route-coverage smoke"));
        assertTrue(created.name().equals(name) && created.accessLevel() == 1);
        assertTrue(ops.retrieval.listCollections().stream()
                .anyMatch(c -> c.id().equals(created.id())));
        var fetched = ops.retrieval.getCollection(created.id());
        assertTrue(fetched.compartments().equals(List.of("cov"))
                && "route-coverage smoke".equals(fetched.description()));

        // Chronology rules: apply (upsert) + verbatim readback.
        String rules = "apiVersion: munarium.ioka.io/v1\nkind: ChronologyRules\n"
                + "metadata: { name: cov-java-rules }\nspec:\n  order:\n"
                + "    - { before: founding.date, after: dissolution.date }\n";
        var applied = ops.runbooks.applyChronologyRules(rules);
        assertTrue(applied.name().equals("cov-java-rules") && applied.ruleCount() == 2);
        assertEquals(rules, ops.runbooks.getChronologyRules("cov-java-rules"),
                "rules must read back verbatim");

        // Findings: empty on a fresh lineage, bogus severity typed-rejected.
        String v = ops.commands.createVersion();
        assertTrue(ops.query.findings(v, Params.FindingsQuery.severity("block")).isEmpty());
        assertThrows(InvalidInputException.class,
                () -> ops.query.findings(v, Params.FindingsQuery.severity("bogus")));
    }

    @Test
    @Order(9)
    void turnStreamSse() {
        assertNotNull(bobToken, "application scenario did not run");
        try (MunariumClient bob = rest(bobToken, "comp-bob")) {
            var session = bob.sessions.create("ent-support");
            var progress = new AtomicInteger();
            var result = bob.sessions.turnStream(session.sessionId(),
                    Params.TurnOptions.of("vacation"), p -> progress.incrementAndGet());
            assertTrue(progress.get() >= 1,
                    "expected at least one progress event (retrieval/merge)");
            assertTrue(!result.hits().isEmpty() && result.ordinal() >= 1,
                    "streamed done must carry the full TurnResult");

            // Closed session: the typed session-not-open refusal — it may
            // land pre-stream OR as the stream's terminal error event
            // (proven live on the Rust port); both decode identically.
            assertEquals("closed", bob.sessions.close(session.sessionId()).state());
            var e = assertThrows(MunariumException.class, () -> bob.sessions.turnStream(
                    session.sessionId(), Params.TurnOptions.of("x"), p -> {}));
            assertInstanceOf(InvalidInputException.class, e,
                    "closed-session refusal must be the typed session-not-open");
        }
    }

    @Test
    @Order(10)
    void grpcSurface() {
        String mgmt = Env.requireMgmt();
        String rw = Env.TOKEN != null ? Env.TOKEN : "devtoken";
        try (MunariumClient gMgr = Env.grpc(mgmt, "mgr")) {
            // Token trio over AdminService.
            var minted = gMgr.tokens.mint(new Tokens.IssueTokenRequest(
                    "grpc-java-user", 2, List.of("eng"), List.of("query"), null, null));
            assertTrue(gMgr.tokens.list(Params.TokenListQuery.forUid("grpc-java-user")).stream()
                    .anyMatch(t -> t.jti().equals(minted.jti())),
                    "grpc-minted token must appear in the audit");

            // Collections trio over RetrievalService (rw static token).
            try (MunariumClient gOps = Env.grpc(rw, "ops")) {
                String name = "cov-grpc-java-" + Env.nonce();
                var created = gOps.retrieval.createCollection(
                        Params.CollectionSpec.of(name, "entdocs@1"));
                assertEquals(name, gOps.retrieval.getCollection(created.id()).name());
                assertTrue(gOps.retrieval.listCollections().stream()
                        .anyMatch(c -> c.id().equals(created.id())));
            }

            // SessionService round-trip with the minted JWT.
            try (MunariumClient user = Env.grpc(minted.token(), "grpc-java-user")) {
                var session = user.sessions.create("ent-support");
                var turn = user.sessions.turn(session.sessionId(),
                        Params.TurnOptions.of("vacation"));
                assertTrue(!turn.hits().isEmpty() && !turn.envelopes().isEmpty(),
                        "grpc turn must carry hits + envelopes");
                assertEquals(1, user.sessions.get(session.sessionId()).turns().size());
                assertEquals("closed", user.sessions.close(session.sessionId()).state());

                // The honest Unsupported set.
                assertThrows(UnsupportedTransportException.class, () -> user.sessions.turnStream(
                        session.sessionId(), Params.TurnOptions.of("x"), p -> {}));
            }
            assertThrows(UnsupportedTransportException.class,
                    () -> gMgr.reports.usage(Params.UsageQuery.byUid()));
            assertThrows(UnsupportedTransportException.class, () -> gMgr.authoring.listPatterns());

            // Revoke last so the earlier calls ran under a live token.
            assertTrue(gMgr.tokens.revoke(minted.jti()).revoked());
        }
    }
}
