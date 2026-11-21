# SPDX-License-Identifier: Apache-2.0
"""Unit tests for the local compatibility harness logic (standard library).

Run from the repository root: ``py -m unittest discover -s server/tools/compat``.
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import compat_local  # noqa: E402
import compat_wire as cw  # noqa: E402
import count_stub  # noqa: E402
import released_clients  # noqa: E402


def entry(cid: str, status: int, body: object, media: str = "application/json", **extra: object) -> dict:
    raw = json.dumps(body).encode()
    resp = cw.decode_body(status, media, raw)
    return dict({"id": cid, "response": resp}, **extra)


def diff(base: list, cand: list, stable: set | None = None) -> dict:
    return cw.summarize(cw.Comparator(stable_ids=stable or set()).compare_transcripts(base, cand))


class TemplatingTests(unittest.TestCase):
    def test_whole_placeholder_keeps_type(self) -> None:
        self.assertEqual(cw.substitute({"h": "${n}"}, {"n": 7}), {"h": 7})
        self.assertEqual(cw.substitute("a-${n}", {"n": 7}), "a-7")

    def test_missing_variable_is_an_error(self) -> None:
        with self.assertRaises(KeyError):
            cw.substitute("${nope}", {})

    def test_capture_where_and_missing(self) -> None:
        case = {
            "capture": {"x": "/a", "y": "/missing"},
            "capture_where": {"o": {"array": "/steps", "match": {"name": "cut"}, "field": "ordinal"}},
        }
        resp = {"json": {"a": 1, "steps": [{"name": "b", "ordinal": 0}, {"name": "cut", "ordinal": 4}]}}
        variables: dict = {}
        missing = cw.capture(case, resp, variables)
        self.assertEqual(variables, {"x": 1, "o": 4})
        self.assertEqual(missing, ["y"])

    def test_render_path_quotes_but_keeps_refs(self) -> None:
        self.assertEqual(cw.render_path("/v1/runbooks/{name}/sessions", {"name": "starter@1"}), "/v1/runbooks/starter@1/sessions")
        self.assertEqual(cw.render_path("/v1/x/{id}", {"id": "a b"}), "/v1/x/a%20b")


class SseTests(unittest.TestCase):
    def test_parse_events_and_json(self) -> None:
        text = ": comment\nevent: progress\ndata: {\"stage\": \"retrieval\"}\n\nevent: done\ndata: {\"ok\": true}\n\n"
        self.assertEqual(
            cw.parse_sse(text),
            [{"event": "progress", "data": {"stage": "retrieval"}}, {"event": "done", "data": {"ok": True}}],
        )

    def test_unterminated_last_event_is_kept(self) -> None:
        self.assertEqual(cw.parse_sse("data: plain"), [{"event": "message", "data": "plain"}])


class ComparatorTests(unittest.TestCase):
    def test_release_metadata_requires_both_expected_versions(self) -> None:
        for first, second, passed in (
            ("1.4.0", "1.5.0", True),
            ("1.4.0", "1.4.0", False),
            ("1.5.0", "1.5.0", False),
            (None, "1.5.0", False),
        ):
            comp = cw.Comparator(release_versions=("1.4.0", "1.5.0"))
            base = {"version": first} if first else {}
            findings = comp.compare_transcripts(
                [entry("meta-version", 200, base)],
                [entry("meta-version", 200, {"version": second})],
            )
            self.assertEqual(cw.summarize(findings)["passed"], passed)

    def test_identical_passes(self) -> None:
        self.assertTrue(diff([entry("a", 200, {"x": [1, 2]})], [entry("a", 200, {"x": [1, 2]})])["passed"])

    def test_added_member_is_reported_not_failed(self) -> None:
        s = diff([entry("a", 200, {"x": 1})], [entry("a", 200, {"x": 1, "new": None})])
        self.assertTrue(s["passed"])
        self.assertEqual(s["counts"], {"added": 1})

    def test_removed_member_fails(self) -> None:
        self.assertFalse(diff([entry("a", 200, {"x": 1, "y": 2})], [entry("a", 200, {"x": 1})])["passed"])

    def test_absent_null_and_empty_are_distinct(self) -> None:
        for b, c in (({"x": None}, {"x": []}), ({"x": []}, {"x": {}}), ({"x": ""}, {"x": None}), ({"x": []}, {"x": [1]})):
            with self.subTest(b=b, c=c):
                self.assertFalse(diff([entry("a", 200, b)], [entry("a", 200, c)])["passed"])

    def test_status_and_media_type(self) -> None:
        self.assertFalse(diff([entry("a", 200, {})], [entry("a", 201, {})])["passed"])
        self.assertFalse(diff([entry("a", 200, {})], [entry("a", 200, {}, media="text/plain")])["passed"])

    def test_enum_value_change_fails(self) -> None:
        s = diff([entry("a", 200, {"status": "accepted"})], [entry("a", 200, {"status": "provisional"})])
        self.assertEqual(s["failing"][0]["kind"], "value_changed")

    def test_generated_ids_must_be_a_consistent_bijection(self) -> None:
        base = [entry("a", 200, {"id": "claim-" + "1" * 32}), entry("b", 200, {"ref": "claim-" + "1" * 32})]
        good = [entry("a", 200, {"id": "claim-" + "9" * 32}), entry("b", 200, {"ref": "claim-" + "9" * 32})]
        bad = [entry("a", 200, {"id": "claim-" + "9" * 32}), entry("b", 200, {"ref": "claim-" + "8" * 32})]
        self.assertTrue(diff(base, good)["passed"])
        self.assertEqual(diff(base, bad)["failing"][0]["kind"], "id_binding")

    def test_two_ids_cannot_map_to_one(self) -> None:
        base = [entry("a", 200, {"p": "ses-" + "1" * 32, "q": "ses-" + "2" * 32})]
        cand = [entry("a", 200, {"p": "ses-" + "9" * 32, "q": "ses-" + "9" * 32})]
        self.assertFalse(diff(base, cand)["passed"])

    def test_stable_seed_ids_must_match_exactly(self) -> None:
        seed = "memv-" + "a" * 32
        s = diff([entry("a", 200, {"v": seed})], [entry("a", 200, {"v": "memv-" + "b" * 32})], stable={seed})
        self.assertFalse(s["passed"])

    def test_timestamps_and_tokens_compare_by_format(self) -> None:
        jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ4In0.sig"
        base = [entry("a", 200, {"t": "2026-11-20 12:57:53.488886+00", "u": "2026-11-20T13:57:55Z", "k": jwt})]
        same = [entry("a", 200, {"t": "2026-11-21 01:00:00.1+00", "u": "2026-11-21T00:00:00Z", "k": jwt + "x"})]
        self.assertTrue(diff(base, same)["passed"])
        self.assertFalse(diff(base, [entry("a", 200, {"t": "yesterday", "u": "2026-11-21T00:00:00Z", "k": jwt})])["passed"])

    def test_latency_numbers_are_volatile_others_are_not(self) -> None:
        self.assertTrue(diff([entry("a", 200, {"latency_ms": 2})], [entry("a", 200, {"latency_ms": 9})])["passed"])
        self.assertFalse(diff([entry("a", 200, {"seq": 2})], [entry("a", 200, {"seq": 3})])["passed"])

    def test_problem_detail_is_informational(self) -> None:
        media = "application/problem+json"
        base = [entry("a", 400, {"type": "t", "title": "x", "status": 400, "detail": "old words"}, media=media)]
        cand = [entry("a", 400, {"type": "t", "title": "x", "status": 400, "detail": "new words"}, media=media)]
        s = diff(base, cand)
        self.assertTrue(s["passed"])
        self.assertEqual(s["counts"], {"informational": 1})
        cand[0]["response"]["json"]["type"] = "other"
        self.assertFalse(diff(base, cand)["passed"])

    def test_exact_cases_compare_bytes(self) -> None:
        base = [entry("a", 200, {"x": 1}, exact=True)]
        cand = [entry("a", 200, {"x": 1})]
        cand[0]["response"]["sha256"] = "different"
        self.assertEqual(diff(base, cand)["failing"][0]["kind"], "bytes")

    def test_volatile_paths_keep_kind(self) -> None:
        base = [entry("a", 200, {"version": "1.4.0"}, volatile=["/json/version"])]
        self.assertTrue(diff(base, [entry("a", 200, {"version": "1.5.0"})])["passed"])
        self.assertFalse(diff(base, [entry("a", 200, {"version": None})])["passed"])

    def test_missing_case_and_skips_fail(self) -> None:
        self.assertFalse(diff([entry("a", 200, {})], [])["passed"])
        self.assertFalse(diff([{"id": "a", "skipped": "x"}], [entry("a", 200, {})])["passed"])

    def test_concurrent_retrieval_progress_is_order_insensitive(self) -> None:
        def ev(*cols: str) -> list:
            return [{"event": "progress", "data": {"stage": "retrieval", "collection": c}} for c in cols] + [
                {"event": "progress", "data": {"stage": "merge"}}, {"event": "done", "data": {}}]
        sse = "text/event-stream"

        def mk(events: list) -> dict:
            return {"id": "s", "response": {"status": 200, "media_type": sse, "sha256": "", "events": events}}

        self.assertTrue(diff([mk(ev("a", "b"))], [mk(ev("b", "a"))])["passed"])
        self.assertFalse(diff([mk(ev("a", "b"))], [mk(ev("a"))])["passed"])
        moved = ev("a", "b")
        moved.insert(0, moved.pop(2))  # merge before retrieval: an ordering change outside the run
        self.assertFalse(diff([mk(ev("a", "b"))], [mk(moved)])["passed"])

    def test_list_length_change_fails(self) -> None:
        self.assertFalse(diff([entry("a", 200, {"h": [1, 2]})], [entry("a", 200, {"h": [1]})])["passed"])


class RunnerTests(unittest.TestCase):
    class Fake:
        name = "rest"  # status expectations apply to the REST transport only

        def __init__(self) -> None:
            self.calls: list = []
            self.states = ["running", "running", "done"]

        def call(self, req: dict) -> dict:
            self.calls.append(req)
            if req["path"].startswith("/poll"):
                return cw.decode_body(200, "application/json", json.dumps({"state": self.states.pop(0)}).encode())
            return cw.decode_body(201, "application/json", json.dumps({"id": "x-1"}).encode())

    def test_capture_poll_skip_and_expectations(self) -> None:
        fake = self.Fake()
        cases = [
            {"id": "make", "method": "POST", "path": "/make", "json": {"n": "${n}"}, "capture": {"made": "/id"}, "expect_status": 200},
            {"id": "use", "method": "GET", "path": "/use/{id}", "params": {"id": "${made}"}},
            {"id": "poll", "method": "GET", "path": "/poll", "poll": {"pointer": "/state", "until": ["done"], "interval": 0}},
            {"id": "skip", "method": "GET", "path": "/x/{id}", "params": {"id": "${undefined}"}},
            {"id": "other", "method": "GET", "path": "/y", "skip_transports": ["rest"]},
        ]
        t = cw.run_cases(cases, fake, {"n": 3})
        self.assertEqual([e["id"] for e in t], ["make", "use", "poll", "skip"])
        self.assertEqual(fake.calls[0]["body"], b'{"n":3}')
        self.assertEqual(fake.calls[1]["path"], "/use/x-1")
        self.assertEqual(t[2]["response"]["json"], {"state": "done"})
        self.assertIn("skipped", t[3])
        self.assertEqual(cw.status_expectations(t), [{"case": "make", "expected": 200, "observed": 201}])


class ProtoRouteTests(unittest.TestCase):
    def test_routes_and_streaming(self) -> None:
        text = """service ServerApiService {
  // POST /v1/sessions/{id}/turns
  rpc Turn(ServerApiRequest) returns (ServerApiResponse);
  // POST /v1/sessions/{id}/turns/stream
  rpc TurnStream(ServerApiRequest) returns (stream ServerApiResponse);
}"""
        self.assertEqual(
            cw.load_proto_routes(text),
            {("POST", "/v1/sessions/{id}/turns"): ("Turn", False), ("POST", "/v1/sessions/{id}/turns/stream"): ("TurnStream", True)},
        )

    def test_v14_proto_maps_every_replay_route(self) -> None:
        proto = Path(__file__).resolve().parents[2] / "proto" / "mmp" / "v1" / "server_api.proto"
        routes = cw.load_proto_routes(proto.read_text(encoding="utf-8"))
        cases = cw.load_json(str(Path(__file__).resolve().parent / "cases" / "replay-v14.json"))["cases"]
        unmapped = [c["id"] for c in cases if "bridge" not in c.get("skip_transports", []) and (c.get("method", "GET"), c["path"]) not in routes]
        self.assertEqual(unmapped, [])


class StubTests(unittest.TestCase):
    def test_ollama_and_openai_shapes(self) -> None:
        self.assertEqual(count_stub.respond("GET", "/api/tags", None)[0], 200)
        status, body = count_stub.respond("POST", "/api/embed", {"input": ["a", "b"]})
        self.assertEqual((status, len(body["embeddings"]), len(body["embeddings"][0])), (200, 2, count_stub.DIM))
        self.assertEqual(count_stub.respond("POST", "/api/embed", {"input": "a"})[1]["embeddings"], [count_stub.embed("a")])
        self.assertEqual(count_stub.respond("POST", "/v1/chat/completions", {})[1]["choices"][0]["message"]["content"], "stub answer")
        self.assertEqual(count_stub.respond("GET", "/nope", None)[0], 404)

    def test_tally(self) -> None:
        lines = [json.dumps({"client": "a", "method": "GET", "path": "/api/tags"})] * 2 + [""]
        self.assertEqual(count_stub.tally(lines), {"a": {"GET /api/tags": 2}})


class ReleasedClientTests(unittest.TestCase):
    def test_csproj_swaps_project_for_package(self) -> None:
        text = '<ItemGroup>\n<ProjectReference Include="..\\..\\src\\Ioka.Munarium.Client\\Ioka.Munarium.Client.csproj" />\n</ItemGroup>'
        out = released_clients.patch_csproj(text)
        self.assertIn('<PackageReference Include="Ioka.Munarium.Client" Version="1.4.0" />', out)
        self.assertNotIn("ProjectReference", out)
        with self.assertRaises(ValueError):
            released_clients.patch_csproj("<Project />")

    def test_manifests_pin_the_released_version(self) -> None:
        self.assertIn('munarium-client = "=1.4.0"', released_clients.rust_manifest())
        self.assertNotIn("path =", released_clients.rust_manifest().split("[dependencies]")[1])
        self.assertIn('io.ioka.munarium:munarium-client:1.4.0', released_clients.java_build())


class PdfTests(unittest.TestCase):
    def test_xref_offsets_point_at_objects(self) -> None:
        pdf = compat_local.minimal_pdf(["a (b)", "c"])
        start = int(pdf.rsplit(b"startxref\n", 1)[1].split(b"\n")[0])
        self.assertTrue(pdf[start:].startswith(b"xref\n0 6\n"))
        rows = pdf[start:].split(b"\n")[3:8]
        for n, row in enumerate(rows, start=1):
            off = int(row[:10])
            self.assertTrue(pdf[off:].startswith(f"{n} 0 obj".encode()), n)
        self.assertIn(b"(a \\(b\\)) Tj", pdf)


if __name__ == "__main__":
    unittest.main()
