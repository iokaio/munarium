# SPDX-License-Identifier: Apache-2.0
"""Live campaign controls: fair baselines, held-out criteria, failures and ownership."""

import copy
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

import frozen_eval as ev
import frozen_eval_pilot
import p13_cases as cases
import p13_live as live
import p13_report
from p13_local import CallError, Client, LocalRig


class CampaignTests(unittest.TestCase):
    def test_note_policy_correction_conflict_history_removal(self):
        data = cases.fixture("pilot", 1)
        expected = {
            "stable": ["amber-000"],
            "correction": ["violet-000"],
            "past": ["amber-000"],
            "conflict": ["amber-000", "violet-000"],
            "removed": [],
            "revoked": ["amber-000"],
        }
        for history in data["histories"]:
            with self.subTest(family=history["family"]):
                selected = cases.resolve_notes(history["events"], history["as_of"])
                self.assertEqual(
                    [e["value"] for e in selected], expected[history["family"]]
                )

    def test_pilot_and_heldout_have_disjoint_history_ids(self):
        pilot = cases.fixture("pilot", 2)
        heldout = cases.fixture("held_out", 10)
        self.assertFalse(
            {h["id"] for h in pilot["histories"]}
            & {h["id"] for h in heldout["histories"]}
        )
        self.assertEqual(len(heldout["histories"]), 60)
        self.assertEqual(len(heldout["cases"]), 300)

    def test_extra_stale_evidence_is_a_failure(self):
        data = cases.fixture("pilot", 1)
        case = next(c for c in data["cases"] if c["family"] == "correction")
        history = next(h for h in data["histories"] if h["family"] == "correction")
        score = ev.grader.grade_case(case, cases.respond(history["events"]), True)
        self.assertIn(
            ("must_not_cite", False), [(x[0], x[1]) for x in score["evidence"]]
        )

    def test_all_arms_preserve_current_authorization_for_historical_query(self):
        history = next(
            h
            for h in cases.fixture("pilot", 1)["histories"]
            if h["family"] == "revoked"
        )
        for arm in cases.ARMS:
            client = Mock()
            client.call.side_effect = CallError(404)
            turn, observations = live.query_arm(
                client, "scoped-token", history, arm, {"version": "v"}
            )
            self.assertEqual(turn["hits"], [])
            self.assertEqual(turn["completion"]["text"], "insufficient evidence")
            self.assertFalse(observations["current_authorization"])
            self.assertEqual(client.call.call_count, 1)

    def test_unexpected_denial_is_not_a_successful_abstention(self):
        history = cases.fixture("pilot", 1)["histories"][0]
        client = Mock()
        client.call.side_effect = CallError(404)
        with self.assertRaises(CallError):
            live.query_arm(
                client, "scoped-token", history, "munarium", {"version": "v"}
            )

    def test_unauthorized_response_never_reaches_responder(self):
        history = next(
            h
            for h in cases.fixture("pilot", 1)["histories"]
            if h["family"] == "revoked"
        )
        client = Mock()
        client.call.return_value = {
            "hits": [{"source_path": history["events"][0]["id"]}]
        }
        with patch.object(cases, "respond") as responder:
            _, observations = live.query_arm(client, "token", history, "munarium", {})
        responder.assert_not_called()
        self.assertEqual(observations["unauthorized_disclosures"], 1)

    def test_context_budget(self):
        with self.assertRaisesRegex(ValueError, "context_budget_exceeded"):
            cases.respond([{"id": "a", "value": "x" * 5000}])

    def summary_fixture(self, correct_baseline=True):
        data = cases.fixture("held_out", 10)
        manifest = {
            "cases": data["cases"],
            "purpose": "qualification",
            "decision_d6": {
                "minimum_supported_correctness": 0.95,
                "minimum_paired_improvement": 0.05,
            },
        }
        raw = {
            "metadata": {"cleanup_completed": True},
            "rows": [
                {
                    "id": c["id"],
                    "status": "completed",
                    "observations": {"unauthorized_disclosures": 0},
                }
                for c in data["cases"]
            ],
        }
        grading = {
            "scores": [
                {
                    "id": c["id"],
                    "outcome": "passed"
                    if correct_baseline or c["arm"] != "conventional_retrieval"
                    else "failed",
                }
                for c in data["cases"]
            ]
        }
        return manifest, raw, grading

    def test_tie_rejects_improvement_claim(self):
        report = live.paired_summary(*self.summary_fixture())
        self.assertEqual(report["d6_verdict"], "rejected")
        self.assertEqual(report["paired_difference"], 0)
        self.assertEqual(report["independent_histories"], 60)
        self.assertFalse(report["model_quality_qualified"])

    def test_clear_effect_can_qualify_but_disclosure_cannot(self):
        manifest, raw, grading = self.summary_fixture(False)
        self.assertEqual(
            live.paired_summary(manifest, raw, grading)["d6_verdict"], "qualified"
        )
        row = next(r for r in raw["rows"] if r["id"].endswith(":munarium"))
        row["observations"]["unauthorized_disclosures"] = 1
        self.assertEqual(
            live.paired_summary(manifest, raw, grading)["d6_verdict"], "rejected"
        )

    def test_missing_rows_are_not_valid_negative_evidence(self):
        manifest, raw, grading = self.summary_fixture(False)
        raw["rows"][0]["status"] = "unexecuted"
        self.assertEqual(
            live.paired_summary(manifest, raw, grading)["d6_verdict"], "incomplete"
        )

    def test_run_level_failure_overrides_positive_and_negative_verdicts(self):
        for correct_baseline in (True, False):
            for metadata in (
                {"runner_error": "ValueError", "cleanup_completed": True},
                {"runner_error": "KeyboardInterrupt", "cleanup_completed": True},
                {"cleanup_completed": False},
                {},
            ):
                with self.subTest(baseline=correct_baseline, metadata=metadata):
                    manifest, raw, grading = self.summary_fixture(correct_baseline)
                    completed = live.paired_summary(manifest, raw, grading)
                    raw["metadata"] = metadata
                    original = copy.deepcopy((raw, grading))
                    report = live.paired_summary(manifest, raw, grading)
                    self.assertEqual(report["d6_verdict"], "incomplete")
                    self.assertEqual(report["per_arm"], completed["per_arm"])
                    self.assertEqual((raw, grading), original)

    def test_host_drift_refuses_before_creating_resources_or_outputs(self):
        host = live.host_identity()
        manifest = frozen_eval_pilot.freeze()
        manifest["build"] = {"binary_sha256": "expected", **host}
        manifest = ev.seal(
            "manifest",
            **{
                k: v
                for k, v in manifest.items()
                if k not in ("kind", "schema_version", "id")
            },
        )
        for field in host:
            changed = {**host, field: "different"}
            with (
                self.subTest(field=field),
                tempfile.TemporaryDirectory() as tmp,
                patch.object(live, "sources", return_value=manifest["source"]),
                patch.object(live, "binary_hash", return_value="expected"),
                patch.object(live, "host_identity", return_value=changed),
                patch.object(live, "LocalRig") as rig,
            ):
                with self.assertRaisesRegex(ValueError, f"live_host_mismatch:{field}"):
                    live.run(manifest, "unused", Path(tmp) / "results", "control")
                rig.assert_not_called()
                self.assertEqual(list(Path(tmp).iterdir()), [])

    def test_http_budget_fails_before_request(self):
        client = Client("http://127.0.0.1:1", "test", limit=0)
        with (
            patch("urllib.request.urlopen") as opening,
            self.assertRaisesRegex(RuntimeError, "request_limit_reached"),
        ):
            client.call("GET", "/v1/anything")
        opening.assert_not_called()

    def test_owned_cleanup_only_and_no_ambient_product_credentials(self):
        with (
            tempfile.TemporaryDirectory() as tmp,
            patch.dict("os.environ", {"MUNARIUM_SECRET_OPENAI": "never-inherit"}),
        ):
            rig = LocalRig(Path(tmp) / "server", Path(tmp) / "owned")
            self.assertNotIn("MUNARIUM_SECRET_OPENAI", rig.environment)
            rig.docker = Mock()
            rig.close()
            rig.docker.assert_not_called()
            rig.container_created = True
            rig.close()
            rig.docker.assert_called_once_with("rm", "--force", "--volumes", rig.name)
            self.assertFalse(rig.container_created)

    def test_failed_fast_measurements_do_not_calibrate_a_budget(self):
        manifest = {"latency_workloads": {"retrieval": {"sample_count": 1}}}
        measurements = {
            "retrieval": [
                {
                    "elapsed_ms": 1,
                    "samples": [{"outcome": "failed", "duration_ms": 0.001}],
                }
            ]
        }
        report = live.calibration(manifest, measurements)["retrieval"]
        self.assertFalse(report["complete"])
        self.assertIsNone(report["proposed_absolute_p95_ms"])

    def test_sustained_relative_regression_requires_two_consecutive_repetitions(self):
        manifest = {
            "latency_workloads": {"retrieval": {}},
            "latency_policy": {
                "absolute_p95_ms": {"retrieval": 100},
                "baseline_p95_ms": {"retrieval": 10},
                "relative_limit": 0.2,
                "consecutive_repetitions": 2,
            },
        }

        def report(values):
            measurements = {
                "retrieval": [
                    {
                        "elapsed_ms": 100,
                        "samples": [{"outcome": "passed", "duration_ms": value}],
                    }
                    for value in values
                ]
            }
            return live.calibration(manifest, measurements)["retrieval"][
                "frozen_budget"
            ]

        self.assertEqual(report([13, 10, 13])["verdict"], "within_local_budget")
        self.assertEqual(report([10, 13, 13])["verdict"], "regressed")
        self.assertTrue(report([101, 10, 10])["absolute_breach"])

    def test_all_unexecuted_latency_slots_survive_interruption(self):
        manifest = {
            "runner_settings": {"repetitions": 3},
            "latency_workloads": {"retrieval": {"sample_count": 5}},
        }
        plan = live.measurement_plan(manifest)
        self.assertEqual(sum(len(r["samples"]) for r in plan["retrieval"]), 15)
        self.assertFalse(live.calibration(manifest, plan)["retrieval"]["complete"])
        p13_report.validate_measurements(manifest, plan)
        plan["retrieval"][0]["samples"].pop()
        with self.assertRaisesRegex(ValueError, "latency_sample_identity"):
            p13_report.validate_measurements(manifest, plan)

    def test_evidence_values_come_from_verified_server_bytes(self):
        history = cases.fixture("pilot", 1)["histories"][0]
        client = Mock()
        client.call.return_value = {
            "hits": [
                {
                    "source_path": history["events"][0]["id"],
                    "text": '{"value":"corrupted"}',
                }
            ]
        }
        with self.assertRaisesRegex(ValueError, "retrieved_evidence_mismatch"):
            live.query_arm(client, "token", history, "conventional_retrieval", {})

    def test_missing_preregistration_refuses_before_starting_resources(self):
        fixture = cases.fixture("held_out", 1)
        manifest = frozen_eval_pilot.freeze()
        manifest.update(
            purpose="qualification",
            cases=fixture["cases"],
            query_hash=ev.digest(
                [{"id": c["id"], "ask": c["ask"]} for c in fixture["cases"]]
            ),
            corpus_hash=ev.digest(fixture["histories"]),
            decision_d6={"selected": True},
            runner_settings={"per_family": 1},
            build={"binary_sha256": "expected", **live.host_identity()},
        )
        manifest = ev.seal(
            "manifest",
            **{
                k: v
                for k, v in manifest.items()
                if k not in ("kind", "schema_version", "id")
            },
        )
        with (
            patch.object(live, "sources", return_value=manifest["source"]),
            patch.object(live, "binary_hash", return_value="expected"),
            patch.object(live, "LocalRig") as rig,
            self.assertRaisesRegex(ValueError, "missing_frozen_commit"),
        ):
            live.run(manifest, "unused", "unused", "control")
        rig.assert_not_called()


if __name__ == "__main__":
    unittest.main()
