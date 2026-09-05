# SPDX-License-Identifier: Apache-2.0
"""Figure specs for matrix/docs/guides/technical/. One function per figure;
the key is the SVG's filename. Rendered by scripts/diagrams.py."""
from diagrams import Svg, node, heading, caption


def matrix_reference_architecture() -> Svg:
    s = Svg(940, 380, "Reference architecture")
    heading(s, "Munarium Matrix — reference architecture")
    zones = [
        (20, "Users and apps", ["application users", "internal portals",
                                "analysts"], "band"),
        (204, "Munarium Server", ["research profile", "evidence hierarchy",
                                  "document retrieval", "citation verification"], "plain"),
        (388, "Munarium Matrix", ["control · query", "sync · reconcile",
                                  "policy · canonicalization", "evidence sealing"], "accent"),
        (572, "Structured sources", ["PostgreSQL", "MySQL", "SQL Server",
                                     "landing exports"], "ok"),
        (756, "Document corpora", ["files, PDFs, email,", "reports, web content"], "band"),
    ]
    for x, title, body, kind in zones:
        node(s, x, 60, 164, 170, title, body, kind=kind)
    for x in (184, 368, 552, 736):
        s.arrow(x, 145, x + 18, 145)
    node(s, 388, 258, 164, 62, "Matrix PostgreSQL",
         ["registry, journal,", "queues, checkpoints"])
    s.arrow(470, 230, 470, 256)
    node(s, 620, 258, 300, 62, "Enterprise adapters",
         ["Databricks · Snowflake · BigQuery · Cube · dbt",
          "not in this repository — registered at runtime"], kind="warn")
    s.arrow(654, 268, 640, 200)
    caption(s, "A core build refuses an Enterprise adapter by name: adapter_not_available.")
    return s


def matrix_asset_lifecycle() -> Svg:
    s = Svg(920, 340, "Asset lifecycle")
    heading(s, "Assets, and the order they must exist in")
    chain = [("DataSource", "connection, egress,\ncredentialRef, authorization"),
             ("DataView /\nMetricView", "the contract:\nwhat may be asked"),
             ("Mapping", "where results land,\nand under whose authority"),
             ("Run", "sealed evidence,\njournalled")]
    x = 30
    for title, body in chain:
        node(s, x, 66, 200, 118, title, body.split("\n"))
        if x > 30:
            s.arrow(x - 14, 124, x - 4, 124)
        x += 214
    states = ["validate", "apply", "probe", "introspect", "publish"]
    bx = 30
    for st in states:
        node(s, bx, 218, 160, 40, st, kind="band")
        if bx > 30:
            s.arrow(bx - 12, 238, bx - 3, 238)
        bx += 172
    caption(s, "Resolve every posture or schema refusal before a contract is written.")
    return s


def matrix_mode_selection() -> Svg:
    s = Svg(900, 360, "Mode selection")
    heading(s, "Choosing an integration mode")
    node(s, 330, 58, 240, 46, "what is the question?", kind="accent")
    node(s, 30, 140, 260, 120, "Mode A — materialize",
         ["the corpus should be", "searchable alongside", "documents",
          "→ snapshot, watermark, CDC"])
    node(s, 320, 140, 260, 120, "Mode B — query",
         ["an exact, bounded result", "belongs in an answer",
          "at request time", "→ contract, compile, seal"])
    node(s, 610, 140, 260, 120, "Mode C — reconcile",
         ["one system may correct", "a canonical property",
          "over effective dates", "→ authority + promotion"], kind="warn")
    s.arrow(400, 106, 200, 138)
    s.arrow(450, 106, 450, 138)
    s.arrow(500, 106, 700, 138)
    node(s, 130, 288, 640, 46, "These are distinct assets with distinct budgets and authority — not one broad credential.",
         kind="band")
    caption(s, "Mode B working is not a reason to promote to Mode C.")
    return s


def matrix_mode_a_pipeline() -> Svg:
    s = Svg(940, 300, "Mode A pipeline")
    heading(s, "Mode A: checkpointed materialization")
    steps = [("read", "snapshot · watermark · CDC"),
             ("canonicalize", "declared types,\nexact decimals"),
             ("chunk", "row → citable unit"),
             ("upload", "to a Server collection"),
             ("checkpoint", "committed only after\nthe upload lands")]
    x = 16
    for title, body in steps:
        node(s, x, 68, 172, 116, title, body.split("\n"),
             kind="ok" if title == "checkpoint" else "plain")
        if x > 16:
            s.arrow(x - 12, 126, x - 3, 126)
        x += 184
    node(s, 120, 214, 700, 54, "Acceptance",
         ["the same batch twice from one checkpoint produces identical events; then an unchanged source produces none"],
         kind="band")
    caption(s, "A gap is reported as incomplete and resnapshotted — never quietly skipped.")
    return s


def matrix_mode_b_pipeline() -> Svg:
    s = Svg(940, 300, "Mode B pipeline")
    heading(s, "Mode B: governed query and sealed evidence")
    steps = [("contract", "the only shape\nthat may be asked"),
             ("compile", "one plan, hashed\nover the parsed AST"),
             ("bind", "parameters bind;\nnever interpolated"),
             ("execute", "under the effective\nprincipal, at limits"),
             ("seal", "evidence block +\nmanifest")]
    x = 16
    for title, body in steps:
        node(s, x, 68, 172, 116, title, body.split("\n"),
             kind="accent" if title == "seal" else "plain")
        if x > 16:
            s.arrow(x - 12, 126, x - 3, 126)
        x += 184
    node(s, 120, 214, 700, 54, "Never infer success from HTTP 200 on verify — inspect `failed`.",
         kind="warn")
    caption(s, "Strings preserve decimals and counts until the application applies its own type.")
    return s


def matrix_mode_c_lifecycle() -> Svg:
    s = Svg(900, 330, "Mode C lifecycle")
    heading(s, "Mode C: reconciliation and controlled correction")
    states = [("observe", "read the source's\nview of a property"),
              ("compare", "identity match,\nvalue conformance"),
              ("propose", "a correction, with\nits authority scope"),
              ("promote", "only inside declared\neffective dates")]
    x = 30
    for title, body in states:
        node(s, x, 66, 194, 116, title, body.split("\n"),
             kind="warn" if title == "promote" else "plain")
        if x > 30:
            s.arrow(x - 13, 124, x - 4, 124)
        x += 208
    node(s, 30, 216, 400, 62, "Promotion requires",
         ["measured identity precision, value conformance,",
          "an authority-scope review, a decision record"], kind="band")
    node(s, 470, 216, 400, 62, "and a tested rollback",
         ["a correction you cannot undo is not a correction"], kind="band")
    caption(s, "Authority is declared per property, not granted per credential.")
    return s


def matrix_runtime_enforcement() -> Svg:
    s = Svg(940, 340, "Runtime enforcement")
    heading(s, "The request pipeline, and where it refuses")
    steps = ["tenant + role", "asset resolve", "authorization class",
             "egress allowlist", "credential resolve", "limits", "seal"]
    x = 14
    for st in steps:
        node(s, x, 70, 126, 56, st)
        if x > 14:
            s.arrow(x - 10, 98, x - 2, 98)
        x += 132
    for i, why in enumerate(["cross-tenant", "unknown asset", "class mismatch",
                             "host not allowed", "missing credential",
                             "over ceiling", "unmodelled type"]):
        node(s, 14 + i * 132, 152, 126, 52, "refuse", [why], kind="warn")
        s.arrow(77 + i * 132, 128, 77 + i * 132, 150)
    node(s, 180, 240, 580, 56, "Every refusal carries a class and a code",
         ["the class says whether a retry can help; the code says what to change"],
         kind="accent")
    caption(s, "A refusal is data. Retry only when the class says it is retryable.")
    return s


def matrix_security_boundaries() -> Svg:
    s = Svg(920, 340, "Security boundaries")
    heading(s, "Three credentials, three different questions")
    node(s, 30, 66, 270, 130, "Session authorization",
         ["carried from Munarium Server", "decides what the request",
          "MAY ASK FOR"], kind="band")
    node(s, 325, 66, 270, 130, "Matrix tenant / role token",
         ["used on Matrix's own API", "decides which tenant and",
          "which OPERATIONS"], kind="accent")
    node(s, 620, 66, 270, 130, "Source credentialRef",
         ["resolved only at call time", "decides what the engine",
          "will actually EXPOSE"], kind="ok")
    node(s, 130, 226, 660, 62, "Do not collapse them into a shared superuser secret",
         ["Matrix also validates the declared authorization class and denied columns before sealing"],
         kind="warn")
    caption(s, "Three boundaries, because they fail differently and are revoked separately.")
    return s


FIGURES = {
    "matrix-reference-architecture": matrix_reference_architecture,
    "matrix-asset-lifecycle": matrix_asset_lifecycle,
    "matrix-mode-selection": matrix_mode_selection,
    "matrix-mode-a-pipeline": matrix_mode_a_pipeline,
    "matrix-mode-b-pipeline": matrix_mode_b_pipeline,
    "matrix-mode-c-lifecycle": matrix_mode_c_lifecycle,
    "matrix-runtime-enforcement": matrix_runtime_enforcement,
    "matrix-security-boundaries": matrix_security_boundaries,
}
