# SPDX-License-Identifier: Apache-2.0
"""Offline regression tests for the worked example's grading guarantees."""
import contextlib
import copy
import io
import json
import pathlib
import sys
import tempfile
import unittest
from unittest.mock import patch

EXAMPLE = pathlib.Path(__file__).resolve().parents[1] / "docs" / "lab" / "example"
sys.path.insert(0, str(EXAMPLE))
import grade
import lab


class GraderTests(unittest.TestCase):
    def setUp(self):
        self.key = json.loads((EXAMPLE / "answer-key.json").read_text(encoding="utf-8"))

    def test_saved_turn_retains_evidence_and_index_identity(self):
        turn = {
            "session_id": "session-1", "ordinal": 1,
            "hits": [{"source_path": "manual.md", "text": "Evidence", "score": 0.5}],
            "envelopes": [{"index_version": "index-1"}],
            "hierarchy": {"extension": "retained"},
            "_permitted_collections": ["manuals"],
        }
        result = grade.grade_case(self.key["cases"][0], turn, False)
        self.assertEqual(result["turn"], {k: v for k, v in turn.items() if not k.startswith("_")})
        self.assertEqual(result["ask"], self.key["cases"][0]["ask"])
        self.assertEqual(result["permitted_collections"], ["manuals"])

    def test_cli_exit_codes_and_saved_results(self):
        case = self.key["cases"][3]
        for complete, completion, hits, expected in [
            (False, None, [], 0),
            (True, None, [], 2),
            (True, {"text": "insufficient evidence"}, [], 0),
            (True, {"text": "The cause was a pump fault."}, [], 1),
            (False, None, [{"source_path": "halvard/engineering/memo.md"}], 1),
            (True, None, [{"source_path": "halvard/engineering/memo.md"}], 1),
        ]:
            with self.subTest(complete=complete, completion=completion, hits=hits), tempfile.TemporaryDirectory() as tmp:
                key_path, out_path = pathlib.Path(tmp) / "key.json", pathlib.Path(tmp) / "out.json"
                key_path.write_text(json.dumps({"runbook": "test@1", "cases": [case]}), encoding="utf-8")
                turn = {"hits": hits, "completion": completion, "session_id": "fresh"}
                args = ["--mgmt-token", "test", "--key", str(key_path), "--out", str(out_path)]
                with patch.object(grade, "mint", return_value="scoped"), \
                        patch.object(grade, "run_turn", return_value=(turn, None)), \
                        contextlib.redirect_stdout(io.StringIO()):
                    code = grade.main(args + (["--complete"] if complete else []))
                self.assertEqual(code, expected)
                saved = json.loads(out_path.read_text(encoding="utf-8"))
                self.assertEqual(saved[0]["turn"], turn)
                self.assertEqual(saved[0]["hard_fail"], bool(hits))

    def ledger_responses(self):
        spec = self.key["ledger"]
        prefix = f"{spec['subject']}.{spec['key']}="
        first = {"id": "first", "seq": 1, "status": "accepted", "normalized_text": prefix + spec["first_value"]}
        second = {"id": "second", "seq": 2, "status": "disputed", "normalized_text": prefix + spec["conflicting_value"]}
        correction = {"id": "correction", "seq": 3, "status": "accepted", "normalized_text": prefix + spec["expect_canon_after_review"]}
        finding = {"rule_id": "gate.ledger-conflict", "severity": "block"}
        return [copy.deepcopy(response) for response in [
            {"version_id": "version"},
            {"claim": first, "findings": []},
            {"claim": second, "findings": [finding]},
            {"facts": [second]},
            {"claim": correction, "findings": []},
            {"facts": [correction]},
            {"facts": [first], "as_of_seq": 1, "head_seq": 3},
            {"findings": [{"seq": 2, "finding": finding}]},
        ]]

    def test_ledger_checks_history_findings_and_exclusive_canon(self):
        for defect in [None, "old canon still accepted", "wrong historical value", "ignored pin",
                       "missing returned finding", "missing persisted finding", "wrong finding sequence",
                       "wrong disputed claim", "rejected correction"]:
            with self.subTest(defect=defect):
                responses = self.ledger_responses()
                if defect == "old canon still accepted":
                    responses[5]["facts"].append(responses[1]["claim"])
                elif defect == "wrong historical value":
                    responses[6]["facts"] = responses[5]["facts"]
                elif defect == "ignored pin":
                    responses[6]["as_of_seq"] = 0
                elif defect == "missing returned finding":
                    responses[2]["findings"] = []
                elif defect == "missing persisted finding":
                    responses[7]["findings"] = []
                elif defect == "wrong finding sequence":
                    responses[7]["findings"][0]["seq"] = 3
                elif defect == "wrong disputed claim":
                    responses[3]["facts"][0]["id"] = "unrelated"
                elif defect == "rejected correction":
                    responses[4]["claim"]["status"] = "disputed"
                with patch.object(lab.Server, "call", side_effect=responses) as call:
                    verdict, detail = grade.grade_ledger("http://unused", "test", self.key)
                self.assertEqual(verdict, "FAIL" if defect else "PASS", detail)
                self.assertEqual(call.call_args_list[6].kwargs["query"], {"as_of_seq": 1})


if __name__ == "__main__":
    unittest.main()
