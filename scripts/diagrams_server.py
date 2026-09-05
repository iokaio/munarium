# SPDX-License-Identifier: Apache-2.0
"""Figure specs for server/docs/guides/. One function per figure; the name of
the key is the SVG's filename. Rendered by scripts/diagrams.py."""
from diagrams import Svg, node, heading, caption


def ch1_system_context() -> Svg:
    s = Svg(880, 300, "System context")
    heading(s, "One contract, two implementations")
    node(s, 40, 60, 220, 130, "The semantics",
         ["An append-only fact ledger,", "deterministic gates, pins,",
          "a budget-aware composer.", "Settled before the server."], kind="band")
    node(s, 330, 60, 220, 130, "Munarium Protocol",
         ["proto/mmp/v1/", "the normative contract",
          "between both worlds"], kind="accent")
    node(s, 620, 60, 220, 130, "munarium-server",
         ["server/src/", "production Rust",
          "REST :8080 / gRPC :50051", "PostgreSQL ledger",
          "object-store source bytes"], kind="ok")
    s.arrow(262, 125, 328, 125, label="define")
    s.arrow(552, 125, 618, 125, label="implement")
    node(s, 610, 215, 240, 52, "client libraries",
         ["clients/ — Rust, Python, .NET, Java"])
    s.arrow(730, 213, 730, 194, label="speak MMP")
    caption(s, "The protos are normative: the two implementations never share code.")
    return s


def ch1_plane_parity() -> Svg:
    s = Svg(820, 330, "Plane parity")
    heading(s, "One scenario set, both wires")
    node(s, 320, 55, 180, 46, "mmp-conformance", kind="accent")
    node(s, 90, 140, 210, 60, "rest.rs", ["HTTP/JSON, problem+json"])
    node(s, 520, 140, 210, 60, "grpc.rs", ["HTTP/2, tonic::Status"])
    s.arrow(370, 103, 220, 138)
    s.arrow(450, 103, 610, 138)
    node(s, 305, 232, 210, 46, "service.rs", ["one implementation"])
    s.arrow(210, 202, 380, 230)
    s.arrow(610, 202, 440, 230)
    node(s, 305, 292, 210, 30, "munarium-core", kind="ok")
    s.arrow(410, 280, 410, 290)
    caption(s, "Each transport translates and funnels: parity is a property, not a promise.")
    return s


def ch2_port_map() -> Svg:
    s = Svg(880, 330, "Port map")
    heading(s, "The port landscape")
    node(s, 40, 60, 250, 118, "Canonical",
         ["443  gateway (TLS)", "8080  REST", "50051  direct gRPC",
          "9090  ops / metrics"], kind="accent", mono_body=True)
    node(s, 315, 60, 250, 118, "From source",
         ["18080  REST", "15051  gRPC", "19090  ops", "18443  gateway"],
         mono_body=True)
    node(s, 590, 60, 250, 118, "Test tiers",
         ["18080/15051/19090  black-box", "18081/19091  platform",
          "5433  Postgres (host)", "9000/9001  MinIO"], mono_body=True)
    node(s, 40, 205, 800, 62, "The reaping rule",
         ["A script reaps only a stale munarium-server on the ALTERNATE ports,",
          "and refuses to touch anything else on the machine."], kind="warn")
    caption(s, "Alternate ports exist so a from-source run never fights a compose stack.")
    return s


def ch3_test_ladder() -> Svg:
    s = Svg(900, 330, "The test ladder")
    heading(s, "Five tiers, cheapest signal first")
    rungs = [
        ("(default)", "offline unit tests +\nin-process conformance", "ok"),
        ("-Postgres", "adds real storage", "plain"),
        ("-BlackBox", "adds both wire planes", "plain"),
        ("-Platform", "adds the pg-backed\nplatform surface", "plain"),
        ("-Cluster", "two servers, one database", "accent"),
    ]
    x = 30
    for label, body, kind in rungs:
        node(s, x, 70, 158, 120, label, body.split("\n"), kind=kind)
        if x > 30:
            s.arrow(x - 12, 130, x - 2, 130)
        x += 172
    node(s, 30, 220, 842, 58, "Gated satellites",
         ["MUNARIUM_TEST_DATABASE_URL, the provider keys and the OCR models each gate a suite.",
          "Unset is a loud skip, never a silent pass."], kind="band")
    caption(s, "-All runs every tier the laptop can run.")
    return s


def ch4_crate_map() -> Svg:
    s = Svg(900, 360, "The workspace by layer")
    heading(s, "Twenty crates, dependencies flowing down")
    layers = [
        ("binaries", ["munarium-server", "munarium-cli"], "accent"),
        ("surface", ["munarium-api-types", "munarium-api-conv", "munarium-proto"], "plain"),
        ("capability", ["munarium-retrieval", "munarium-runbooks", "munarium-authoring",
                        "munarium-providers", "munarium-shapes", "munarium-access"], "plain"),
        ("adapters", ["munarium-store-pg", "munarium-store-mem", "munarium-store-objects",
                      "munarium-retrieval-pg", "munarium-datastore", "munarium-extract"], "plain"),
        ("kernel", ["munarium-core"], "ok"),
    ]
    y = 58
    for name, crates, kind in layers:
        node(s, 60, y, 700, 52, name, [" · ".join(crates)], kind=kind)
        if y > 58:
            s.arrow(410, y - 8, 410, y - 1)
        y += 62
    s.rect(778, 58, 96, 246, fill="band")
    s.text(826, 92, "mmp-", size=12, mono=True)
    s.text(826, 108, "conformance", size=12, mono=True)
    s.lines(826, 140, ["in-process", "and over", "both wires"], size=11)
    s.text(826, 292, "no upward", size=11, fill="warn")
    caption(s, "The upward direction is forbidden: a lower layer never names a higher one.")
    return s


def ch5_startup_order() -> Svg:
    s = Svg(760, 470, "Startup order")
    heading(s, "main.rs, in order")
    steps = [
        ("`openapi` argv short-circuit", "prints the spec, exits 0", "band"),
        ("tracing init", "MUNARIUM_LOG", "plain"),
        ("Config::from_env", "exit 2 — config error", "warn"),
        ("MUNARIUM_GRPC_ADDR parse", "exit 2 — config error", "warn"),
        ("AppState::new", "exit 1 — startup error", "warn"),
        ("REST listener :8080", "", "ok"),
        ("direct gRPC listener :50051", "", "ok"),
        ("ops listener :9090", "bind failure is fatal", "ok"),
    ]
    y = 56
    for title, body, kind in steps:
        node(s, 210, y, 340, 40, title, [body] if body else (), kind=kind)
        if y > 56:
            s.arrow(380, y - 10, 380, y - 2)
        y += 50
    caption(s, "Two exit codes, and they mean different things: 2 is your config, 1 is the world.")
    return s


def ch6_change_surface() -> Svg:
    s = Svg(880, 400, "One change, many surfaces")
    heading(s, "One change, many surfaces")
    node(s, 350, 165, 180, 70, "one change",
         ["a route, an RPC,", "a migration, a slug"], kind="accent")
    around = [
        (40, 60, "rest.rs router"), (40, 150, "grpc.rs impls"),
        (40, 240, "openapi.rs"), (40, 330, "generated API docs"),
        (680, 60, "doc registries"), (680, 150, "README env table"),
        (680, 240, "deny.toml"), (680, 330, "conformance list"),
        (350, 330, "crate tests"),
    ]
    for x, y, label in around:
        node(s, x, y, 160, 44, label)
        sx = x + 160 if x < 350 else x
        s.arrow(sx if x < 350 else x, y + 22, 348 if x < 350 else 532, 190 if y < 200 else 215)
    caption(s, "The recipes in §6 exist so none of these is remembered rather than checked.")
    return s


def ch7_conformance_contexts() -> Svg:
    s = Svg(880, 300, "Conformance contexts")
    heading(s, "One scenario set, five contexts")
    ctx = [
        ("in-process", "MemStore"), ("in-process", "PgStore, fresh tenant"),
        ("adapter", "REST client"), ("adapter", "gRPC client"),
        ("platform", "pg-backed, live server"),
    ]
    x = 30
    for a, b in ctx:
        node(s, x, 70, 160, 92, a, [b], kind="accent" if a == "adapter" else "plain")
        x += 172
    s.rect(374, 178, 344, 40, fill="okfill", stroke="ok")
    s.text(546, 203, "the paired adapter run is the parity check", size=12, fill="ok")
    caption(s, "The same harness runs against any deployed server from the command line.")
    return s


def ch8_data_tiers() -> Svg:
    s = Svg(880, 350, "The two data tiers")
    heading(s, "Two data tiers, one bridge")
    node(s, 40, 60, 340, 150, "PostgreSQL",
         ["kernel: versions, claims, lineage_heads",
          "retrieval: chunks, collections, indexes",
          "platform: uids, tokens, sessions, reports"], kind="accent")
    node(s, 500, 60, 340, 150, "Object store",
         ["az · s3 · gcs · file · pg · mem",
          "one adapter over object_store",
          "credentials from the ambient chain"], kind="ok")
    s.rect(300, 240, 280, 48, fill="band")
    s.text(440, 262, "sources.storage_backend", size=12, mono=True)
    s.text(440, 278, "sources.blob_uri", size=12, mono=True)
    s.arrow(300, 264, 220, 214)
    s.arrow(580, 264, 660, 214)
    caption(s, "Index versions are immutable slabs; cutover moves one pointer.")
    return s


def ch9_dependency_gauntlet() -> Svg:
    s = Svg(920, 300, "The dependency gauntlet")
    heading(s, "Six gates, in order")
    gates = ["licence allow-list", "advisories", "openssl / native-tls ban",
             "unknown-source denial", "alpine musl static link", "distroless size budget"]
    x = 24
    for g in gates:
        node(s, x, 74, 138, 80, g)
        if x > 24:
            s.arrow(x - 12, 114, x - 2, 114)
        x += 150
    node(s, 24, 186, 430, 62, "stock features",
         ["fail at the musl gate: cmake enters the builder"], kind="warn")
    node(s, 490, 186, 406, 62, "-base + ring",
         ["clears all six"], kind="ok")
    caption(s, "A dependency is a decision, and the gauntlet is where the decision is made.")
    return s


def ch11_triage_tree() -> Svg:
    s = Svg(900, 400, "Triage")
    heading(s, "Triage: start with what the process did")
    node(s, 40, 62, 380, 30, "the server will not start", kind="warn")
    node(s, 40, 108, 380, 56, "exit 2 — config error",
         ["fix the variable the message names"])
    node(s, 40, 176, 380, 56, "exit 1 — startup error",
         ["the world refused: connectivity, credentials"])
    node(s, 40, 244, 380, 70, "migration checksum mismatch",
         ["an applied migration's bytes changed;",
          "recreate the database (compose down -v locally)"])
    node(s, 480, 62, 380, 30, "a request failed", kind="warn")
    rows = [("400", "uid-required / invalid-input"), ("401", "unauthenticated"),
            ("403", "forbidden — level or compartment"),
            ("404", "unknown asset or version"),
            ("409", "head conflict — retry at the observed head"),
            ("422", "gate refusal — read the finding")]
    y = 108
    for code, meaning in rows:
        s.rect(480, y, 380, 34, fill="paper")
        s.text(510, y + 22, code, size=13, mono=True, weight="600", anchor="middle")
        s.text(552, y + 22, meaning, size=11, fill="muted", anchor="start")
        y += 40
    caption(s, "A status code narrows it to one page of the error registry.")
    return s


def ch12_honesty_stack() -> Svg:
    s = Svg(820, 380, "The honesty rule at five layers")
    heading(s, "One rule, five layers")
    rows = [
        ("gates", "a blocked claim is disputed, never dropped"),
        ("API surface", "UNIMPLEMENTED rather than a faked RPC"),
        ("CI", "a server that never became ready is an error, not a skip"),
        ("docs", "gaps stated in a ledger, not omitted"),
        ("conformance", "a scenario that cannot run says SKIPPED, loudly"),
    ]
    y = 62
    for name, body in rows:
        node(s, 60, y, 700, 54, name, [body], kind="ok" if y == 62 else "plain")
        y += 62
    caption(s, "The failure this prevents is the green that measured nothing.")
    return s


def ch14_division_of_labor() -> Svg:
    s = Svg(900, 340, "The division of labour")
    heading(s, "Where your application ends and the substrate begins")
    node(s, 30, 60, 260, 170, "Your application",
         ["the end user", "your IdP / API manager",
          "— the security boundary —", "your UX"], kind="band")
    node(s, 330, 60, 240, 60, "POST /v1/access-tokens",
         ["a capability JWT"], kind="accent")
    node(s, 330, 140, 240, 170, "Governed data planes",
         ["/v1/runbooks/{name}/sessions", "/v1/sessions/{id}/turns",
          "/v1/search", "/v1/ingest",
          "— filtered by level", "and compartments —"], mono_body=True)
    node(s, 610, 60, 260, 250, "Munarium",
         ["the ledger", "the gates", "point-in-time pins",
          "uid-attributed interactions", "provenance envelopes"], kind="ok")
    s.arrow(292, 92, 328, 92)
    s.arrow(292, 200, 328, 200)
    s.arrow(572, 200, 608, 200)
    caption(s, "Munarium is not your identity provider, and does not try to be.")
    return s


def ch15_five_stages() -> Svg:
    s = Svg(940, 300, "Five stages")
    heading(s, "A corpus application, end to end")
    stages = [
        ("corpus files", "the filename IS the identity"),
        ("POST /v1/ingest", "source rows + object store"),
        ("POST /v1/shapes", "chunk and index rules, in git"),
        ("POST /v1/runbooks", "collections: levels + prefixes"),
        ("the run", "resolve → build → verify →\napproval → retireOld"),
        ("sessions / search", "every answer carries a\nprovenance envelope"),
    ]
    x = 20
    for title, body in stages:
        node(s, x, 70, 145, 130, title, body.split("\n"))
        if x > 20:
            s.arrow(x - 11, 122, x - 2, 122)
        x += 154
    caption(s, "index_version · event_watermark · source_content_hashes — on every answer.")
    return s


def ch16_clearance_filter() -> Svg:
    s = Svg(880, 350, "The clearance filter")
    heading(s, "Same runbook, same question, different tokens")
    node(s, 340, 58, 200, 56, "briefing@1", ["two collections"], kind="accent")
    node(s, 100, 140, 260, 56, "press-public", ["accessLevel 0"])
    node(s, 520, 140, 260, 56, "finance-internal",
         ["accessLevel 2 + finance"], kind="warn")
    s.arrow(400, 116, 260, 138)
    s.arrow(480, 116, 620, 138)
    node(s, 100, 232, 260, 84, "level 0, no compartments",
         ["permits press-public only", "→ 2 hits, 1 envelope"])
    node(s, 520, 232, 260, 84, "level 2 with finance",
         ["permits both", "→ 4 hits, 2 envelopes"], kind="ok")
    caption(s, "permits() runs BEFORE ranking: a denied chunk is never scored, never seen.")
    return s


def ch17_session_turn() -> Svg:
    s = Svg(900, 360, "A session timeline")
    heading(s, "The server carries no conversation")
    s.rect(30, 54, 380, 250, fill="band", dash="6 4")
    s.text(220, 76, "client owns conversation state", size=12, fill="muted")
    for i, msg in enumerate(["“what did the harbour report say?”",
                             "“and the quarter before that?”",
                             "“who signed it?”  → rewritten"]):
        node(s, 50, 96 + i * 68, 340, 52, f"message {i + 1}", [msg])
    node(s, 500, 54, 370, 84, "session",
         ["pins harbor-desk@1", "snapshots level 1 at creation"], kind="accent")
    node(s, 500, 154, 370, 96, "each turn, independently",
         ["snapshot-filtered retrieval over",
          "harbor-press and harbor-filings",
          "→ hits + one envelope per collection"])
    node(s, 500, 266, 370, 38, "completion — optional, not run here", kind="band")
    s.arrow(412, 180, 496, 180, label="only the query string crosses")
    caption(s, "session_turns stores the transcript for audit; it is not conversation memory.")
    return s


def ch18_beyond_rag() -> Svg:
    s = Svg(900, 380, "Two memory surfaces")
    heading(s, "Two surfaces, one prompt")
    node(s, 40, 62, 360, 130, "Document index",
         ["content-addressed sources",
          "immutable, clearance-filtered",
          "search → hits + provenance",
          "says what documents SAY"], kind="accent")
    node(s, 500, 62, 360, 130, "Canonical memory",
         ["extracted claims meet the gates",
          "accepted → canon + budgeted brief",
          "disputed → human review queue",
          "says what is TRUE NOW"], kind="ok")
    node(s, 620, 208, 240, 54, "review queue",
         ["never enters the brief"], kind="warn")
    s.arrow(680, 194, 700, 206)
    node(s, 250, 286, 400, 68, "your prompt",
         ["known facts + document hits + provenance + the question"], kind="band")
    s.arrow(220, 194, 380, 284)
    s.arrow(680, 194, 520, 284)
    caption(s, "Retrieval never decides truth; the ledger never guesses at wording.")
    return s


def ch20_platform_ring() -> Svg:
    s = Svg(900, 400, "The platform ring")
    heading(s, "What your integration owns")
    s.rect(300, 120, 300, 160, fill="okfill", stroke="ok", dash="7 5")
    s.text(450, 180, "Munarium", size=15, weight="700")
    s.lines(450, 204, ["every table keyed by tenant_id;",
                       "no cross-tenant read path"], size=11)
    ring = [
        (30, 60, "Identity", "your IdP mints capability JWTs;\nTTL ≤ 24 h, revocable"),
        (630, 60, "Observability", "x-munarium-request-id;\n/v1/reports/usage|audit|cost"),
        (30, 300, "Cost governance", "BYOK keys, rpm/tpm budgets,\ntier routing, overrides closed"),
        (630, 300, "Data lifecycle", "double-pass soft removal;\nphysical deletion is a runbook"),
        (330, 330, "Tenancy", "one tenant per key, enforced in every query"),
    ]
    for x, y, title, body in ring:
        node(s, x, y, 240, 76, title, body.split("\n"))
    caption(s, "Five touchpoints. Munarium implements none of them, and says so.")
    return s


def ch21_tutorial_map() -> Svg:
    s = Svg(940, 340, "The tutorial map")
    heading(s, "§21, in eight steps")
    steps = [
        ("1 Stand up", "tokens, a fresh tenant,\nthe uid contract", "§14"),
        ("2 Load", "batch ingest;\neverything bound to nothing yet", "§15"),
        ("3 Shape + run", "collections, steps,\nhuman approvals", "§15–16"),
        ("4 Two clearances", "same question,\ndifferent permitted sets", "§16"),
        ("5 Chat turns", "anaphora rewritten client-side", "§17"),
        ("6 The red flag", "a ledger conflict → queue →\ncorrection → the pin", "§18"),
        ("7 Completion", "BYOK, tier override,\nthe quote check", "§17, §20"),
        ("8 Grade", "an answer key in git,\na harness in CI", "§19"),
    ]
    for i, (title, body, ref) in enumerate(steps):
        x = 20 + (i % 4) * 232
        y = 62 + (i // 4) * 128
        node(s, x, y, 216, 104, title, body.split("\n") + [ref])
        if i % 4:
            s.arrow(x - 14, y + 52, x - 3, y + 52)
    caption(s, "The substrate underneath is unchanged for every application on it.")
    return s


def ch21a_runbook_lifecycle() -> Svg:
    s = Svg(940, 320, "Runbook lifecycle")
    heading(s, "Operating a runbook")
    flow = [("apply", "shape first, then runbook\n(kind-routed upsert)"),
            ("validate", "deterministic findings"),
            ("run --watch", "resolveSources → buildIndex\n→ verify, side by side"),
            ("awaiting_approval", "a human approves the cutover"),
            ("retireOld", "keeps versions for rollback")]
    x = 20
    for title, body in flow:
        node(s, x, 66, 172, 116, title, body.split("\n"),
             kind="warn" if title == "awaiting_approval" else "plain")
        if x > 20:
            s.arrow(x - 13, 124, x - 3, 124)
        x += 186
    node(s, 120, 212, 300, 54, "evolve",
         ["edited YAML re-applied as a new version"], kind="accent")
    node(s, 520, 212, 300, 54, "retire",
         ["double-pass soft removal; no resurrection"], kind="band")
    caption(s, "The approval gate is the only place a human is required, and it is required.")
    return s


def ch21b_authoring_loop() -> Svg:
    s = Svg(940, 320, "The guided authoring loop")
    heading(s, "From pattern to production")
    steps = [("pattern catalog", "pick a precedent"),
             ("draft + interview", "questions in §16 order"),
             ("materialize", "deterministic; per-document\nand set.* findings"),
             ("assist (optional)", "BYOK edit; bad output discarded"),
             ("export", "refuses while errors exist;\nwrites shapes, runbooks, bundle.json"),
             ("git", "the source of truth"),
             ("bundle apply", "verifies every hash;\nshapes first")]
    x = 14
    for i, (title, body) in enumerate(steps):
        node(s, x, 66, 122, 128, title, body.split("\n"),
             kind="ok" if i >= 5 else "plain")
        if x > 14:
            s.arrow(x - 10, 130, x - 2, 130)
        x += 132
    node(s, 200, 218, 540, 54, "any byte drift since export kills the deploy",
         kind="warn")
    caption(s, "What reaches production is exactly the validated set that left authoring.")
    return s


FIGURES = {
    "ch1-system-context": ch1_system_context,
    "ch1-plane-parity": ch1_plane_parity,
    "ch2-port-map": ch2_port_map,
    "ch3-test-ladder": ch3_test_ladder,
    "ch4-crate-map": ch4_crate_map,
    "ch5-startup-order": ch5_startup_order,
    "ch6-change-surface": ch6_change_surface,
    "ch7-conformance-contexts": ch7_conformance_contexts,
    "ch8-data-tiers": ch8_data_tiers,
    "ch9-dependency-gauntlet": ch9_dependency_gauntlet,
    "ch11-triage-tree": ch11_triage_tree,
    "ch12-honesty-stack": ch12_honesty_stack,
    "ch14-division-of-labor": ch14_division_of_labor,
    "ch15-five-stages": ch15_five_stages,
    "ch16-clearance-filter": ch16_clearance_filter,
    "ch17-session-turn": ch17_session_turn,
    "ch18-beyond-rag": ch18_beyond_rag,
    "ch20-platform-ring": ch20_platform_ring,
    "ch21-tutorial-map": ch21_tutorial_map,
    "ch21a-runbook-lifecycle": ch21a_runbook_lifecycle,
    "ch21b-authoring-loop": ch21b_authoring_loop,
}
