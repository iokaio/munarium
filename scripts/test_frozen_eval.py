# SPDX-License-Identifier: Apache-2.0
"""Independent controls for immutable evaluation and explicitly measured claims."""

import copy
import json
import tempfile
import unittest
from pathlib import Path

import frozen_eval as ev
import frozen_eval_pilot as pilot


def reseal(record):
    record = copy.deepcopy(record)
    record["id"] = ev.digest({k: v for k, v in record.items() if k != "id"})
    return record


class FrozenTests(unittest.TestCase):
    def setUp(self):
        self.manifest = pilot.freeze()
        fixture = ev.read(pilot.FIXTURE)
        self.raw = ev.seal(
            "raw",
            manifest_id=self.manifest["id"],
            run_id="control",
            source=self.manifest["source"],
            purpose="pilot",
            rows=[
                {
                    "id": c["id"],
                    "status": "completed",
                    "reason": "scripted",
                    "usage": None,
                    "turn": fixture["scripted_responses"][c["id"]],
                }
                for c in self.manifest["cases"]
            ],
        )

    def rejected(self, record, reason):
        with self.assertRaisesRegex(ValueError, "^" + reason + "$"):
            ev.grade(self.manifest, reseal(record))

    def test_case_set_controls(self):
        for defect, reason in (
            ("empty", "empty_or_missing_cases"),
            ("duplicate", "duplicate_case_id"),
            ("extra", "extra_case_id"),
            ("missing", "missing_case_id"),
        ):
            with self.subTest(defect=defect):
                raw = copy.deepcopy(self.raw)
                if defect == "empty":
                    raw["rows"] = []
                elif defect == "duplicate":
                    raw["rows"].append(copy.deepcopy(raw["rows"][0]))
                elif defect == "extra":
                    raw["rows"].append({**raw["rows"][0], "id": "extra"})
                else:
                    raw["rows"].pop()
                self.rejected(raw, reason)

    def test_identity_schema_source_and_usage_controls(self):
        for field, value, reason in (
            ("manifest_id", "other", "manifest_identity_mismatch"),
            ("source", {}, "source_mismatch"),
            ("schema_version", 2, "unsupported_schema"),
            ("purpose", "qualification", "purpose_mismatch"),
        ):
            self.rejected({**self.raw, field: value}, reason)
        raw = copy.deepcopy(self.raw)
        raw["rows"][0]["usage"] = {"input_tokens": -1, "output_tokens": 0}
        self.rejected(raw, "invalid_usage")
        raw["rows"][0]["usage"] = {"input_tokens": None, "output_tokens": 0}
        result = ev.grade(self.manifest, reseal(raw))
        self.assertEqual(result["unknown_usage_rows"], len(raw["rows"]))

    def test_semantic_failure_and_malformed_evidence_reasons(self):
        for turn, check in (
            ({}, "evidence_shape"),
            ({"hits": None}, "evidence_shape"),
            ({"hits": [42]}, "evidence_shape"),
            ({"hits": [{}]}, "evidence_shape"),
            ({"hits": [{"source_path": 1}]}, "evidence_shape"),
            ({"hits": [], "completion": {"text": "Friday"}}, "cite_all"),
            (
                {
                    "hits": [{"source_path": "restricted/secret"}],
                    "completion": {"text": "Friday"},
                },
                "must_not_cite",
            ),
            (
                {
                    "hits": [{"source_path": "aster/correction"}],
                    "completion": {"text": "Tuesday"},
                },
                "contains_all",
            ),
        ):
            with self.subTest(turn=turn):
                raw = copy.deepcopy(self.raw)
                raw["rows"][0]["turn"] = turn
                result = ev.grade(self.manifest, reseal(raw))
                self.assertEqual(result["exit_code"], 1)
                checked = result["scores"][0]["checks"]
                self.assertIn(
                    (check, False),
                    [(c[0], c[1]) for c in checked["evidence"] + checked["answer"]],
                )

    def test_opaque_ids_and_unknown_usage_are_not_failures_or_zero(self):
        result = ev.grade(self.manifest, self.raw)
        self.assertEqual(result["exit_code"], 0)
        self.assertEqual(result["scores"][0]["id"], "opaque:α/01")
        self.assertEqual(result["unknown_usage_rows"], 5)

    def test_status_retention_and_failure_precedence(self):
        raw = copy.deepcopy(self.raw)
        for row, status in zip(
            raw["rows"],
            ["failed", "abstained", "interrupted", "unexecuted", "completed"],
        ):
            row["status"] = status
        result = ev.grade(self.manifest, reseal(raw))
        self.assertEqual(result["exit_code"], 1)
        self.assertFalse(result["completed"])
        self.assertEqual(len(result["scores"]), 5)
        self.assertEqual(result["scores"][2]["reason"], "interrupted")
        raw["rows"][0]["status"] = "completed"
        self.assertEqual(ev.grade(self.manifest, reseal(raw))["exit_code"], 3)

    def test_missing_completion_and_malformed_completion(self):
        raw = copy.deepcopy(self.raw)
        raw["rows"][0]["turn"]["completion"] = None
        result = ev.grade(self.manifest, reseal(raw))
        self.assertEqual(result["exit_code"], 3)
        self.assertEqual(result["scores"][0]["reason"], "missing_completion")
        raw["rows"][0]["turn"]["completion"] = {"text": 4}
        self.assertEqual(ev.grade(self.manifest, reseal(raw))["exit_code"], 1)

    def test_no_vacuous_qualification_or_unknown_checks(self):
        for modify, reason in (
            (lambda m: m.update(purpose="qualification"), "missing_decision_d6"),
            (
                lambda m: m["cases"][0].update(expect={"typo": True}),
                "unknown_expectation",
            ),
            (
                lambda m: m["cases"][0].update(expect={"cite_all": []}),
                "invalid_cite_all",
            ),
        ):
            manifest = copy.deepcopy(self.manifest)
            modify(manifest)
            with self.assertRaisesRegex(ValueError, "^" + reason + "$"):
                ev.validate_manifest(reseal(manifest))

    def test_tampering_duplicate_records_and_regrading(self):
        result = ev.grade(self.manifest, self.raw)
        bundle = [self.manifest, self.raw, result]
        ev.verify_bundle(json.loads(json.dumps(bundle)))
        with self.assertRaisesRegex(ValueError, "duplicate_result_id"):
            ev.verify_bundle(bundle + [result])
        bad = copy.deepcopy(result)
        bad["scores"][0]["checks"]["hits"] = ["fabricated"]
        with self.assertRaisesRegex(ValueError, "grading_mismatch"):
            ev.verify_bundle(json.loads(json.dumps([*bundle[:2], reseal(bad)])))
        corrected = ev.grade(self.manifest, self.raw, revision="2")
        self.assertNotEqual(result["id"], corrected["id"])
        self.assertEqual(result["raw_id"], corrected["raw_id"])
        rerun = reseal({**self.raw, "diagnostic_note": "same run identifier"})
        with self.assertRaisesRegex(ValueError, "duplicate_run_id"):
            ev.verify_bundle([self.manifest, self.raw, rerun])

    def test_independent_history_split_and_task_controls(self):
        manifest = copy.deepcopy(self.manifest)
        manifest["cases"][1]["split"] = "held_out"
        with self.assertRaisesRegex(ValueError, "history_split_leakage"):
            ev.validate_manifest(reseal(manifest))
        manifest["cases"][1]["split"] = "pilot"
        manifest["cases"][1]["task_id"] = manifest["cases"][0]["task_id"]
        with self.assertRaisesRegex(ValueError, "duplicate_history_task"):
            ev.validate_manifest(reseal(manifest))

    def test_persisted_producer_receipt_and_immutable_writes(self):
        with tempfile.TemporaryDirectory() as tmp:
            manifest_path = ev.write(tmp, self.manifest)
            manifest = ev.read(manifest_path)
            raw, result = pilot.produce(manifest, tmp, "offline-control")
            self.assertEqual(result["exit_code"], 0)
            ev.verify_bundle([ev.read(p) for p in Path(tmp).glob("*.json")])
            with self.assertRaises(FileExistsError):
                ev.write(tmp, raw)

    def test_interruption_and_provider_failure_are_persisted(self):
        for exception, expected in (
            (KeyboardInterrupt(), 3),
            (RuntimeError("private detail"), 1),
        ):
            with tempfile.TemporaryDirectory() as tmp:

                def provider(case, exception=exception):
                    raise exception

                raw, result = pilot.produce(
                    self.manifest, tmp, "failure-control", provider
                )
                self.assertEqual(result["exit_code"], expected)
                self.assertEqual(len(raw["rows"]), 5)
                self.assertNotIn("private detail", json.dumps(raw))
                if expected == 3:
                    self.assertEqual(raw["rows"][1]["status"], "unexecuted")

    def test_duplicate_json_keys(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bad.json"
            path.write_text('{"id": 1, "id": 2}', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "duplicate_json_key"):
                ev.read(path)

    def test_measured_claim_controls(self):
        result = ev.grade(self.manifest, self.raw)
        source = ev.digest(result["source"])
        marker = f"<!-- measured-result: {result['id']} source: {source} -->"
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            directory = root / "server/conformance/results"
            for record in (self.manifest, self.raw, result):
                ev.write(directory, record)
            doc = root / "README.md"
            for text, reason in (
                (marker, None),
                ("ordinary 100 ms illustration", None),
                (
                    marker.replace(source, "sha256:" + "0" * 64),
                    "result_source_mismatch",
                ),
                (
                    marker.replace(result["id"], "sha256:" + "0" * 64),
                    "missing_result_id",
                ),
                ("<!-- measured-result: bad -->", "malformed_result_marker"),
                (
                    marker.replace(result["id"], self.raw["id"]),
                    "result_schema_mismatch",
                ),
            ):
                doc.write_text(text, encoding="utf-8")
                findings = ev.check_claims(root)
                self.assertEqual(len(findings), 1 if reason else 0, findings)
                if reason:
                    self.assertIn(reason, findings[0])

    def test_nearest_rank_goodput_and_failure_exclusion(self):
        samples = [{"duration_ms": i, "outcome": "passed"} for i in range(1, 101)]
        samples.append({"duration_ms": 0.001, "outcome": "failed"})
        result = ev.latency_report(samples, elapsed_ms=10000)
        self.assertEqual(result["p95_ms"], 95)
        self.assertEqual(result["p50_ms"], 50)
        self.assertEqual(result["goodput_per_second"], 10)
        self.assertEqual(result["offered"], 101)
        self.assertIsNone(ev.latency_report([samples[-1]], elapsed_ms=1)["p95_ms"])
        with self.assertRaisesRegex(ValueError, "invalid_duration"):
            ev.latency_report(
                [{"duration_ms": float("nan"), "outcome": "passed"}], elapsed_ms=1
            )

    def test_three_latency_workloads_and_calibration_metadata(self):
        definition = {
            "build_profile": "release",
            "hardware_class": "fictional-control",
            "concurrency": 1,
            "cache_state": "cold",
            "sample_count": 2,
            "corpus_size": 100,
            "eligible_fraction": 0.5,
            "history_depth": 10,
            "database_bytes": 1024,
            "artifact_bytes": 1024,
            "rerun_policy": "retain_all",
        }
        manifest = {
            "latency_workloads": {
                k: copy.deepcopy(definition)
                for k in ("retrieval", "ledger_append", "cold_readiness")
            }
        }
        measurements = {
            k: {
                "workload_id": ev.digest(v),
                "elapsed_ms": 100,
                "samples": [
                    {
                        "id": str(i),
                        "outcome": "passed",
                        "duration_ms": 10 + i,
                        "stages_ms": {"total": 10 + i},
                        "memory_bytes": None,
                        "candidate_work": None,
                    }
                    for i in range(2)
                ],
            }
            for k, v in manifest["latency_workloads"].items()
        }
        report = ev.validate_latency(manifest, measurements)
        self.assertEqual(report["retrieval"]["p95_ms"], 11)
        self.assertEqual(report["ledger_append"]["calibration"], "report_only")
        for mutate, reason in (
            (lambda m: m.pop("retrieval"), "latency_workload_mismatch"),
            (
                lambda m: m["retrieval"].update(workload_id="wrong"),
                "workload_identity_mismatch",
            ),
            (
                lambda m: m["retrieval"]["samples"].pop(),
                "latency_sample_count_mismatch",
            ),
            (
                lambda m: m["retrieval"]["samples"][0].pop("memory_bytes"),
                "missing_sample_memory_bytes",
            ),
        ):
            bad = copy.deepcopy(measurements)
            mutate(bad)
            with self.assertRaisesRegex(ValueError, "^" + reason + "$"):
                ev.validate_latency(manifest, bad)

    def test_incomplete_claim_and_duplicate_index_rejected(self):
        raw = copy.deepcopy(self.raw)
        raw["rows"][0]["status"] = "interrupted"
        raw = reseal(raw)
        result = ev.grade(self.manifest, raw)
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            directory = root / "server/conformance/results"
            for record in (self.manifest, raw, result):
                ev.write(directory, record)
            (root / "README.md").write_text(
                f"<!-- measured-result: {result['id']} source: {ev.digest(result['source'])} -->",
                encoding="utf-8",
            )
            self.assertIn("incomplete_result", ev.check_claims(root)[0])
            ev.write(root, result)
            (directory / "sha256-duplicate.json").write_text(
                json.dumps(result), encoding="utf-8"
            )
            self.assertIn("duplicate_result_id", ev.check_claims(root)[0])


if __name__ == "__main__":
    unittest.main()
