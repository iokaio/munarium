# SPDX-License-Identifier: Apache-2.0
"""Offline tests for the I3 runner: metrics, verdicts, manifests, answer keys."""

import copy
import json
import math
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import frozen_eval as ev  # noqa: E402
import i3_eval as i3  # noqa: E402

THRESHOLD = {
    "k": 10,
    "min_recall": 0.6,
    "min_ndcg": 0.5,
    "max_p95_ms": 1000,
    "max_failures": 0,
    "max_unauthorized_disclosures": 0,
}


def manifest(thresholds=None, purpose="pilot"):
    files = {
        "docs": [
            {"path": f"d/{n}.md", "sha256": "sha256:" + "0" * 64, "bytes": 1}
            for n in "abc"
        ],
        "code": [
            {"path": f"c/{n}.rs", "sha256": "sha256:" + "1" * 64, "bytes": 1}
            for n in "abc"
        ],
    }
    cases = [
        {
            "id": "d1",
            "family": "docs",
            "kind": "conceptual",
            "query": "q",
            "relevant": {"d/a.md": 2},
        },
        {
            "id": "d2",
            "family": "docs",
            "kind": "identifier",
            "query": "q",
            "relevant": {"d/b.md": 2, "d/c.md": 1},
        },
        {
            "id": "c1",
            "family": "code",
            "kind": "conceptual",
            "query": "q",
            "relevant": {"c/a.rs": 2},
        },
        {
            "id": "c2",
            "family": "code",
            "kind": "paraphrase",
            "query": "q",
            "relevant": {"c/b.rs": 1},
        },
    ]
    profile = {"embedding": {"model": "e"}, "rerank": {"model": "r"}}
    return ev.seal(
        i3.KIND_MANIFEST,
        campaign="test",
        revision="1",
        purpose=purpose,
        source={"x": "sha256:" + "2" * 64},
        build={"server_binary_sha256": "sha256:" + "3" * 64},
        postgres_image=i3.PG_IMAGE,
        families={
            f: {
                "files": files[f],
                "corpus_hash": ev.digest(files[f]),
                "restricted": [files[f][0]["path"]],
                "authorization_queries": [f[0] + "1"],
            }
            for f in i3.FAMILIES
        },
        cases=cases,
        cases_hash=ev.digest(cases),
        arms=list(i3.ARMS),
        candidate_arm="combined",
        k_values=[5, 10],
        primary_k=10,
        profile=profile,
        profile_id=ev.digest(profile),
        thresholds=thresholds,
        resource_limits={"max_wall_seconds": 10},
    )


def good_row(qid, family, paths, ms=100.0):
    return {
        "id": qid,
        "status": "ok",
        "latency_ms": ms,
        "hits": [
            {
                "rank": i,
                "source_path": f"{family}/{p}",
                "chunk_id": f"k{i}",
                "restricted_marker": False,
            }
            for i, p in enumerate(paths)
        ],
        "stages": [],
        "index_version": "idx",
    }


def good_auth(qid):
    return {
        "id": qid,
        "unauthorized": {"outcome": "denied", "status": 404, "hits": 0},
        "control": {"outcome": "passed", "hits": 2},
    }


def raw_for(m, *, rows=None, auth=None, unavailable=(), run_error=None):
    families = {}
    for family in i3.FAMILIES:
        default_rows = {
            "docs": [
                good_row("d1", "docs", ["d/a.md"]),
                good_row("d2", "docs", ["d/b.md", "d/c.md"]),
            ],
            "code": [
                good_row("c1", "code", ["c/a.rs"]),
                good_row("c2", "code", ["c/b.rs"]),
            ],
        }[family]
        arms = {}
        for arm in i3.ARMS:
            key = (family, arm)
            arms[arm] = {
                "availability": "unavailable" if key in unavailable else "available",
                "reason": None,
                "config": [],
                "queries": copy.deepcopy((rows or {}).get(key, default_rows))
                if key not in unavailable
                else [],
                "authorization": copy.deepcopy(
                    (auth or {}).get(key, [good_auth(family[0] + "1")])
                ),
            }
        families[family] = {"builds": {}, "arms": arms, "rollback_replay": None}
    return ev.seal(
        i3.KIND_RAW,
        manifest_id=m["id"],
        run_id="t",
        families=families,
        run_error=run_error,
    )


def receipts_and_verdict(m, raw):
    receipts = i3.family_receipts(m, raw)
    return {r["family"]: r for r in receipts}, i3.two_family_verdict(m, raw, receipts)


class Metrics(unittest.TestCase):
    def test_recall_counts_distinct_relevant_files_in_the_cutoff(self):
        relevant = {"a": 2, "b": 1, "c": 1}
        self.assertEqual(i3.recall_at_k(["a", "x", "b"], relevant, 2), 1 / 3)
        self.assertEqual(i3.recall_at_k(["a", "x", "b"], relevant, 3), 2 / 3)
        self.assertEqual(i3.recall_at_k([], relevant, 10), 0.0)

    def test_binary_ndcg_matches_the_textbook_value(self):
        relevant = {"a": 1, "b": 1}
        value = i3.ndcg_at_k(["x", "a", "b"], relevant, 3)
        dcg = 1 / math.log2(3) + 1 / math.log2(4)
        idcg = 1 + 1 / math.log2(3)
        self.assertAlmostEqual(value, dcg / idcg)
        self.assertEqual(i3.ndcg_at_k(["a", "b"], relevant, 10), 1.0)

    def test_graded_ndcg_prefers_the_higher_grade_first(self):
        relevant = {"a": 2, "b": 1}
        best = i3.ndcg_at_k(["a", "b"], relevant, 2)
        swapped = i3.ndcg_at_k(["b", "a"], relevant, 2)
        self.assertEqual(best, 1.0)
        self.assertAlmostEqual(swapped, (1 + 3 / math.log2(3)) / (3 + 1 / math.log2(3)))
        self.assertLess(swapped, best)

    def test_ndcg_cutoff_ignores_results_below_k(self):
        self.assertEqual(i3.ndcg_at_k(["x", "y", "a"], {"a": 2}, 2), 0.0)

    def test_metrics_refuse_empty_or_invalid_judgments(self):
        for bad in ({}, {"a": 0}):
            with self.assertRaises(ValueError):
                i3.ndcg_at_k(["a"], bad, 5)
        with self.assertRaises(ValueError):
            i3.recall_at_k(["a"], {}, 5)
        with self.assertRaises(ValueError):
            i3.ndcg_at_k(["a"], {"a": 1.5}, 5)
        with self.assertRaises(ValueError):
            i3.recall_at_k(["a"], {"a": 1}, 0)

    def test_file_ranking_deduplicates_and_drops_foreign_prefixes(self):
        hits = [
            {"source_path": p}
            for p in ("docs/a", "docs/a", "docs-restricted/b", "docs/b")
        ]
        ranked = i3.ranked_files(hits, "docs/")
        self.assertEqual(ranked[0], "a")
        self.assertEqual(ranked[2], "b")
        self.assertEqual(len(ranked), 3)
        # A restricted copy of a relevant file never counts as a relevant hit.
        self.assertEqual(i3.recall_at_k(ranked[:2], {"b": 2}, 2), 0.0)

    def test_p95_is_nearest_rank_and_agrees_with_frozen_eval(self):
        values = list(range(1, 21))
        self.assertEqual(i3.nearest_rank(values, 0.95), 19)
        self.assertEqual(i3.nearest_rank([5], 0.95), 5)
        self.assertEqual(i3.nearest_rank(list(range(1, 101)), 0.95), 95)
        self.assertIsNone(i3.nearest_rank([], 0.95))
        samples = [{"outcome": "passed", "duration_ms": float(v)} for v in values]
        # A fast failure cannot improve the percentile.
        samples.append({"outcome": "failed", "duration_ms": 0.0})
        report = ev.latency_report(samples, elapsed_ms=1000.0)
        self.assertEqual(report["p95_ms"], i3.nearest_rank(values, 0.95))


class Verdicts(unittest.TestCase):
    def test_no_thresholds_is_incomplete_never_passed(self):
        m = manifest()
        receipts, verdict = receipts_and_verdict(m, raw_for(m))
        for receipt in receipts.values():
            self.assertEqual(receipt["verdict"], "incomplete")
            self.assertIn("no_thresholds_declared", receipt["reasons"])
            self.assertEqual(receipt["thresholds_status"], "none_declared")
        self.assertEqual(verdict["verdict"], "incomplete")
        self.assertFalse(verdict["claim_both_families"])
        self.assertEqual(verdict["exit_code"], 3)

    def test_two_passing_families_support_both_for_qualification(self):
        m = manifest({f: THRESHOLD for f in i3.FAMILIES}, purpose="qualification")
        receipts, verdict = receipts_and_verdict(m, raw_for(m))
        self.assertEqual(
            {f: r["verdict"] for f, r in receipts.items()},
            {"docs": "passed", "code": "passed"},
        )
        self.assertEqual(verdict["verdict"], "supported")
        self.assertTrue(verdict["claim_both_families"])
        self.assertEqual(verdict["exit_code"], 0)

    def test_a_pilot_cannot_qualify_even_when_every_threshold_passes(self):
        m = manifest({f: THRESHOLD for f in i3.FAMILIES}, purpose="pilot")
        receipts, verdict = receipts_and_verdict(m, raw_for(m))
        self.assertTrue(all(r["verdict"] == "passed" for r in receipts.values()))
        self.assertEqual(verdict["verdict"], "incomplete")
        self.assertFalse(verdict["claim_both_families"])

    def test_one_failed_family_fails_and_blocks_the_both_family_claim(self):
        m = manifest({f: THRESHOLD for f in i3.FAMILIES}, purpose="qualification")
        miss = [good_row("c1", "code", ["c/x.rs"]), good_row("c2", "code", ["c/y.rs"])]
        raw = raw_for(m, rows={("code", "combined"): miss})
        receipts, verdict = receipts_and_verdict(m, raw)
        self.assertEqual(receipts["docs"]["verdict"], "passed")
        self.assertEqual(receipts["code"]["verdict"], "failed")
        self.assertIn("recall@10_below_threshold", receipts["code"]["reasons"])
        self.assertEqual(verdict["verdict"], "failed")
        self.assertEqual(verdict["families"], {"docs": "passed", "code": "failed"})
        self.assertFalse(verdict["claim_both_families"])

    def test_p95_and_failure_thresholds_fail_the_family(self):
        m = manifest({f: THRESHOLD for f in i3.FAMILIES}, purpose="qualification")
        slow = [
            good_row("d1", "docs", ["d/a.md"], ms=5000),
            good_row("d2", "docs", ["d/b.md", "d/c.md"], ms=5000),
        ]
        receipts, _ = receipts_and_verdict(
            m, raw_for(m, rows={("docs", "combined"): slow})
        )
        self.assertIn("p95_above_threshold", receipts["docs"]["reasons"])
        broken = [
            good_row("d1", "docs", ["d/a.md"]),
            {"id": "d2", "status": "failed", "failure": {"class": "http_error"}},
        ]
        receipts, _ = receipts_and_verdict(
            m, raw_for(m, rows={("docs", "combined"): broken})
        )
        self.assertEqual(receipts["docs"]["verdict"], "failed")
        self.assertIn("failures_above_threshold", receipts["docs"]["reasons"])
        self.assertEqual(
            receipts["docs"]["arms"]["combined"]["failure_classes"], {"http_error": 1}
        )

    def test_failure_takes_precedence_over_missing_evidence(self):
        m = manifest({f: THRESHOLD for f in i3.FAMILIES}, purpose="qualification")
        miss = [good_row("d1", "docs", ["d/x.md"]), good_row("d2", "docs", ["d/y.md"])]
        raw = raw_for(
            m, rows={("docs", "combined"): miss}, unavailable={("docs", "reranker")}
        )
        receipts, _ = receipts_and_verdict(m, raw)
        self.assertEqual(receipts["docs"]["verdict"], "failed")
        self.assertIn("arm_unavailable:reranker", receipts["docs"]["reasons"])

    def test_missing_arm_or_unexecuted_query_leaves_the_family_incomplete(self):
        m = manifest({f: THRESHOLD for f in i3.FAMILIES}, purpose="qualification")
        receipts, verdict = receipts_and_verdict(
            m, raw_for(m, unavailable={("code", "reranker")})
        )
        self.assertEqual(receipts["code"]["verdict"], "incomplete")
        self.assertIn("arm_unavailable:reranker", receipts["code"]["reasons"])
        self.assertEqual(verdict["verdict"], "incomplete")
        partial = [
            good_row("d1", "docs", ["d/a.md"]),
            {"id": "d2", "status": "unexecuted"},
        ]
        receipts, _ = receipts_and_verdict(
            m, raw_for(m, rows={("docs", "embedder"): partial})
        )
        self.assertEqual(receipts["docs"]["verdict"], "incomplete")
        self.assertIn("unexecuted_queries:embedder", receipts["docs"]["reasons"])
        receipts, _ = receipts_and_verdict(
            m, raw_for(m, run_error={"class": "harness_error"})
        )
        self.assertEqual(receipts["docs"]["verdict"], "incomplete")
        self.assertIn("run_error", receipts["docs"]["reasons"])

    def test_unexecuted_queries_score_zero_in_the_planned_mean(self):
        m = manifest()
        partial = [good_row("d1", "docs", ["d/a.md"])]
        receipts, _ = receipts_and_verdict(
            m, raw_for(m, rows={("docs", "hash"): partial})
        )
        arm = receipts["docs"]["arms"]["hash"]
        self.assertEqual(arm["metrics"]["recall@10"], 0.5)
        self.assertEqual(arm["metrics_successful_only"]["recall@10"], 1.0)

    def test_any_unauthorized_disclosure_fails_even_without_thresholds(self):
        m = manifest()
        leaked = [
            good_row("d1", "docs", ["d/a.md"]),
            good_row("d2", "docs", ["d/b.md"]),
        ]
        leaked[1]["hits"].append(
            {
                "rank": 1,
                "source_path": "docs-restricted/d/a.md",
                "chunk_id": "z",
                "restricted_marker": True,
            }
        )
        receipts, verdict = receipts_and_verdict(
            m, raw_for(m, rows={("docs", "hash"): leaked})
        )
        self.assertEqual(receipts["docs"]["verdict"], "failed")
        self.assertIn("unauthorized_disclosure:hash", receipts["docs"]["reasons"])
        self.assertEqual(verdict["verdict"], "failed")
        opened = [
            {
                "id": "d1",
                "unauthorized": {"outcome": "allowed_empty", "status": 200, "hits": 0},
                "control": {"outcome": "passed", "hits": 1},
            }
        ]
        receipts, _ = receipts_and_verdict(
            m, raw_for(m, auth={("docs", "embedder"): opened})
        )
        self.assertIn(
            "authorization_not_enforced:embedder", receipts["docs"]["reasons"]
        )

    def test_a_vacuous_authorization_check_is_incomplete(self):
        m = manifest({f: THRESHOLD for f in i3.FAMILIES}, purpose="qualification")
        no_control = [
            {
                "id": "c1",
                "unauthorized": {"outcome": "denied", "status": 404, "hits": 0},
                "control": {"outcome": "failed", "hits": 0},
            }
        ]
        receipts, _ = receipts_and_verdict(
            m, raw_for(m, auth={("code", "hash"): no_control})
        )
        self.assertEqual(receipts["code"]["verdict"], "incomplete")
        self.assertIn(
            "authorization_evidence_incomplete:hash", receipts["code"]["reasons"]
        )
        receipts, _ = receipts_and_verdict(m, raw_for(m, auth={("code", "hash"): []}))
        self.assertIn(
            "authorization_evidence_incomplete:hash", receipts["code"]["reasons"]
        )

    def test_a_pooled_or_single_family_receipt_cannot_claim_both(self):
        m = manifest({f: THRESHOLD for f in i3.FAMILIES}, purpose="qualification")
        raw = raw_for(m)
        receipts = i3.family_receipts(m, raw)
        single = i3.two_family_verdict(
            m, raw, [r for r in receipts if r["family"] == "docs"]
        )
        self.assertEqual(single["verdict"], "incomplete")
        self.assertIn("missing_family_receipt:code", single["reasons"])
        self.assertFalse(single["claim_both_families"])
        pooled = dict(receipts[0], family="pooled")
        with self.assertRaises(ValueError):
            i3.two_family_verdict(m, raw, [pooled])
        with self.assertRaises(ValueError):
            i3.two_family_verdict(m, raw, [receipts[0], receipts[0]])
        other = dict(receipts[1], profile_id="sha256:" + "9" * 64)
        mixed = i3.two_family_verdict(m, raw, [receipts[0], other])
        self.assertEqual(mixed["verdict"], "incomplete")
        self.assertIn("profile_mismatch", mixed["reasons"])

    def test_the_ndcg_gain_threshold_needs_the_hash_baseline(self):
        block = dict(THRESHOLD, min_ndcg_gain_over_hash=0.05)
        m = manifest({f: block for f in i3.FAMILIES}, purpose="qualification")
        receipts, _ = receipts_and_verdict(m, raw_for(m))
        self.assertIn(
            "ndcg@10_gain_over_hash_below_threshold", receipts["docs"]["reasons"]
        )
        receipts, _ = receipts_and_verdict(
            m, raw_for(m, unavailable={("docs", "hash")})
        )
        self.assertIn("no_hash_baseline_for_gain", receipts["docs"]["reasons"])

    def test_stage_receipts_must_match_the_arm(self):
        applied = {"stage": "embedding", "applied": True, "state": "ready"}
        rerank = {"stage": "reranking", "applied": True, "state": "ready"}
        skipped = {"stage": "reranking", "applied": False, "state": "not_applicable"}
        refused = {"stage": "reranking", "applied": False, "state": "unavailable"}
        self.assertIsNone(i3.stage_problem("hash", []))
        self.assertIsNone(i3.stage_problem("combined", [applied, rerank]))
        self.assertIsNone(i3.stage_problem("reranker", [skipped]))
        self.assertEqual(i3.stage_problem("embedder", []), "stage_set_mismatch")
        self.assertEqual(i3.stage_problem("hash", [applied]), "stage_set_mismatch")
        self.assertEqual(
            i3.stage_problem("combined", [applied, refused]), "stage_not_applied"
        )


class Replay(unittest.TestCase):
    def test_replay_differences_are_classified(self):
        before = good_row("d1", "docs", ["d/a.md", "d/b.md"])
        self.assertEqual(
            i3.replay_difference(before, copy.deepcopy(before)), "identical"
        )
        moved = copy.deepcopy(before)
        moved["hits"][0]["score"] = 0.5
        self.assertEqual(i3.replay_difference(before, moved), "scores_changed")
        swapped = copy.deepcopy(before)
        swapped["hits"].reverse()
        self.assertEqual(i3.replay_difference(before, swapped), "order_changed")
        other = good_row("d1", "docs", ["d/c.md"])
        self.assertEqual(i3.replay_difference(before, other), "hits_changed")
        repinned = dict(copy.deepcopy(before), index_version="idx-other")
        self.assertEqual(
            i3.replay_difference(before, repinned), "index_version_changed"
        )
        failed = {"id": "d1", "status": "failed"}
        self.assertEqual(i3.replay_difference(before, failed), "not_comparable")
        self.assertEqual(i3.replay_difference(None, before), "not_comparable")
        summary = i3.replay_summary(
            {
                "queries": [before, moved],
                "differences": {"a": "identical", "b": "scores_changed"},
            }
        )
        self.assertEqual(
            summary,
            {"queries": 2, "differences": {"identical": 1, "scores_changed": 1}},
        )


class Manifests(unittest.TestCase):
    def test_new_qualification_manifest_requires_the_frozen_profile_and_held_out_fixture(self):
        fields = {k: v for k, v in manifest().items()
                  if k not in ("id", "kind", "schema_version")}
        fields.update(
            revision="2", purpose="qualification", candidate_arm="embedder",
            cpu_p95_limit_ms=i3.CPU_P95_LIMIT_MS, fixture="scripts/i3_held_out.json",
            thresholds={f: i3.QUALIFICATION_THRESHOLDS for f in i3.FAMILIES},
        )
        i3.validate_manifest(ev.seal(i3.KIND_MANIFEST, **fields))
        for override in (
            {"candidate_arm": "combined"},
            {"fixture": i3.FIXTURE},
            {"thresholds": {f: THRESHOLD for f in i3.FAMILIES}},
        ):
            with self.subTest(override=override), self.assertRaises(ValueError):
                i3.validate_manifest(ev.seal(i3.KIND_MANIFEST, **(fields | override)))

    def test_new_replay_receipts_recompute_differences_from_exact_baseline(self):
        before = good_row("d1", "docs", ["d/a.md"])
        after = good_row("d1", "docs", ["d/b.md"])
        replay = {
            "baseline_queries": [before],
            "queries": [after],
            "differences": {"d1": "identical"},
        }
        with self.assertRaisesRegex(ValueError, "rollback_replay_difference_mismatch"):
            i3.replay_summary(replay)
        replay["differences"]["d1"] = "scores_changed"
        self.assertEqual(i3.replay_summary(replay)["differences"], {"scores_changed": 1})

    def test_thresholds_must_be_complete_and_keep_disclosures_at_zero(self):
        i3.validate_thresholds(None, [10])
        i3.validate_thresholds({f: THRESHOLD for f in i3.FAMILIES}, [5, 10])
        bad = [
            {"docs": THRESHOLD},
            {f: dict(THRESHOLD, max_unauthorized_disclosures=1) for f in i3.FAMILIES},
            {f: dict(THRESHOLD, k=20) for f in i3.FAMILIES},
            {f: dict(THRESHOLD, min_recall=1.5) for f in i3.FAMILIES},
            {
                f: {k: v for k, v in THRESHOLD.items() if k != "max_p95_ms"}
                for f in i3.FAMILIES
            },
            {f: dict(THRESHOLD, pooled=True) for f in i3.FAMILIES},
        ]
        for thresholds in bad:
            with self.assertRaises(ValueError):
                i3.validate_thresholds(thresholds, [5, 10])

    def test_manifest_identity_detects_any_change(self):
        m = manifest()
        i3.validate_manifest(m)
        changed = copy.deepcopy(m)
        changed["cases"][0]["relevant"] = {"d/b.md": 2}
        with self.assertRaises(ValueError):
            i3.validate_manifest(changed)  # identity no longer matches
        resealed = ev.seal(
            i3.KIND_MANIFEST,
            **{
                k: v
                for k, v in changed.items()
                if k not in ("id", "kind", "schema_version")
            },
        )
        with self.assertRaises(ValueError):
            i3.validate_manifest(resealed)  # judgments no longer match their hash
        fields = {
            k: v for k, v in m.items() if k not in ("id", "kind", "schema_version")
        }
        with self.assertRaises(ValueError):
            i3.validate_manifest(
                ev.seal(i3.KIND_MANIFEST, **dict(fields, arms=["hash"]))
            )
        with self.assertRaises(ValueError):
            i3.validate_manifest(
                ev.seal(i3.KIND_MANIFEST, **dict(fields, purpose="qualification"))
            )
        profile = dict(fields["profile"], top_k=99)
        with self.assertRaises(ValueError):
            i3.validate_manifest(
                ev.seal(i3.KIND_MANIFEST, **dict(fields, profile=profile))
            )

    def test_records_are_immutable_on_disk(self):
        m = manifest()
        with tempfile.TemporaryDirectory() as directory:
            path = ev.write(directory, m)
            self.assertEqual(path.name, m["id"].replace(":", "-") + ".json")
            with self.assertRaises(FileExistsError):
                ev.write(directory, m)
            self.assertEqual(ev.read(path), m)

    def test_verify_recomputes_receipts_and_rejects_tampering(self):
        m = manifest()
        raw = raw_for(m)
        receipts = i3.family_receipts(m, raw)
        verdict = i3.two_family_verdict(m, raw, receipts)
        with tempfile.TemporaryDirectory() as directory:
            for record in [m, raw, *receipts, verdict]:
                ev.write(directory, record)
            self.assertEqual(i3.verify_directory(directory), 3)
        forged = dict(receipts[0], verdict="passed")
        forged = ev.seal(
            i3.KIND_RECEIPT,
            **{
                k: v
                for k, v in forged.items()
                if k not in ("id", "kind", "schema_version")
            },
        )
        with tempfile.TemporaryDirectory() as directory:
            for record in [m, raw, forged, receipts[1], verdict]:
                ev.write(directory, record)
            with self.assertRaises(ValueError):
                i3.verify_directory(directory)


class AnswerKeys(unittest.TestCase):
    def test_glob_matching_is_segment_wise(self):
        self.assertTrue(i3.glob_match("server/docs/ops/a.md", "server/docs/ops/*.md"))
        self.assertFalse(
            i3.glob_match("server/docs/ops/x/a.md", "server/docs/ops/*.md")
        )
        self.assertTrue(i3.glob_match("a/src/x/y.rs", "a/src/**/*.rs"))
        self.assertTrue(i3.glob_match("a/src/y.rs", "a/src/**/*.rs"))
        self.assertFalse(i3.glob_match("b/a/src/y.rs", "a/src/**/*.rs"))

    def test_answer_keys_are_refused_from_any_corpus(self):
        for key in i3.ANSWER_KEYS + ("scripts/i3_other.json",):
            with self.assertRaises(ValueError):
                i3.assert_no_answer_keys(["README.md", key])
        spec = {"include": ["scripts/*.json", "README.md"], "exclude": []}
        with self.assertRaises(ValueError):
            i3.select_corpus(spec, ["README.md", i3.FIXTURE])
        spec = {"include": ["server/docs/guides/*.md"], "exclude": []}
        with self.assertRaises(ValueError):
            i3.select_corpus(spec, ["server/docs/guides/frozen-evaluation.md"])

    def test_the_pilot_fixture_corpus_excludes_its_answer_keys(self):
        fixture = ev.read(i3.ROOT / i3.FIXTURE)
        tracked = i3.tracked_files()
        ids = [q["id"] for q in fixture["queries"]]
        self.assertEqual(len(ids), len(set(ids)))
        for family in i3.FAMILIES:
            spec = fixture["families"][family]
            files = i3.select_corpus(spec, tracked)
            self.assertFalse(set(files) & set(i3.ANSWER_KEYS))
            self.assertTrue(set(spec["restricted"]) <= set(files))
            queries = [q for q in fixture["queries"] if q["family"] == family]
            self.assertGreaterEqual(len(queries), 25)
            for q in queries:
                # Every judgment names a file that is actually in the corpus.
                self.assertTrue(set(q["relevant"]) <= set(files), q["id"])

    def test_the_upload_plan_rechecks_the_manifest(self):
        m = manifest()
        plan = i3.upload_plan(m, "docs")
        self.assertIn(("docs/d/a.md", "d/a.md", False), plan)
        self.assertIn(("docs-restricted/d/a.md", "d/a.md", True), plan)
        tampered = copy.deepcopy(m)
        tampered["families"]["docs"]["files"].append(
            {"path": i3.FIXTURE, "sha256": "sha256:" + "0" * 64, "bytes": 1}
        )
        with self.assertRaises(ValueError):
            i3.upload_plan(tampered, "docs")

    def test_problem_codes_keep_only_plain_slugs(self):
        body = json.dumps(
            {"type": "https://munarium.ioka.io/problems/inference-stage-unavailable"}
        )
        self.assertEqual(i3.problem_code(body.encode()), "inference-stage-unavailable")
        self.assertIsNone(
            i3.problem_code(json.dumps({"type": "a b <script>"}).encode())
        )
        self.assertIsNone(i3.problem_code(b"not json"))


if __name__ == "__main__":
    unittest.main()
