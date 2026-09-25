# SPDX-License-Identifier: Apache-2.0
"""Offline controls for commit/tree/blob inclusion proofs; no Git history required."""

import base64
import copy
import json
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import frozen_eval as ev
import frozen_eval_pilot as pilot
import preregistration as proof


class ProofTests(unittest.TestCase):
    def setUp(self):
        self.manifest = {"id": "sha256:" + "a" * 64, "cases": ["fictional"]}
        self.make_proof()

    def make_proof(self):
        blob = json.dumps(self.manifest).encode()
        identity = proof.object_id("blob", blob)
        trees = []
        for mode, name in [
            (b"100644", self.manifest["id"].replace(":", "-") + ".json"),
            (b"40000", "results"),
            (b"40000", "conformance"),
            (b"40000", "server"),
        ]:
            tree = mode + b" " + name.encode() + b"\0" + bytes.fromhex(identity)
            trees.insert(0, base64.b64encode(tree).decode())
            identity = proof.object_id("tree", tree)
        commit = b"tree " + identity.encode() + b"\n\nFrozen fixture\n"
        self.commit = proof.object_id("commit", commit)
        self.receipt = {
            "commit_object": base64.b64encode(commit).decode(),
            "tree_objects": trees,
            "manifest_blob": base64.b64encode(blob).decode(),
        }

    def test_valid_proof_needs_no_local_git(self):
        proof.verify(self.manifest, self.commit, self.receipt)

    def test_archived_raw_validates_in_shallow_checkout_and_refuses_missing_proof(self):
        manifest = pilot.freeze()
        manifest.update(purpose="qualification", decision_d6={"selected": True})
        for case in manifest["cases"]:
            case["split"] = "held_out"
        self.manifest = ev.seal(
            "manifest",
            **{
                k: v
                for k, v in manifest.items()
                if k not in ("id", "schema_version", "kind")
            },
        )
        self.make_proof()
        raw = ev.seal(
            "raw",
            manifest_id=self.manifest["id"],
            source=self.manifest["source"],
            purpose="qualification",
            run_id="offline-proof-control",
            frozen_commit=self.commit,
            rows=[
                {
                    "id": case["id"],
                    "status": "unexecuted",
                    "reason": "control",
                    "usage": None,
                }
                for case in manifest["cases"]
            ],
        )
        with (
            tempfile.TemporaryDirectory() as tmp,
            patch.object(ev, "ROOT", Path(tmp)),
            patch.object(
                ev.subprocess,
                "run",
                return_value=SimpleNamespace(returncode=128, stdout=b""),
            ),
        ):
            with self.assertRaisesRegex(ValueError, "frozen_manifest_not_committed"):
                ev.validate_raw(self.manifest, raw)
            path = (
                Path(tmp)
                / "server/conformance/results"
                / (
                    "preregistration-"
                    + self.manifest["id"].split(":")[1]
                    + "-"
                    + self.commit
                    + ".json"
                )
            )
            path.parent.mkdir(parents=True)
            path.write_text(json.dumps(self.receipt), encoding="utf-8")
            ev.validate_raw(self.manifest, raw)

    def test_changed_commit_tree_blob_and_manifest_fail(self):
        for target, reason in (
            ("commit", "commit_object_mismatch"),
            ("tree", "tree_object_mismatch"),
            ("blob", "committed_manifest_mismatch"),
            ("manifest", "committed_manifest_mismatch"),
        ):
            with self.subTest(target=target):
                receipt = copy.deepcopy(self.receipt)
                manifest = copy.deepcopy(self.manifest)
                if target == "commit":
                    receipt["commit_object"] = base64.b64encode(b"different").decode()
                elif target == "tree":
                    receipt["tree_objects"][1] = receipt["tree_objects"][0]
                elif target == "blob":
                    receipt["manifest_blob"] = base64.b64encode(b"{}").decode()
                else:
                    manifest["cases"] = []
                with self.assertRaisesRegex(ValueError, reason):
                    proof.verify(manifest, self.commit, receipt)


if __name__ == "__main__":
    unittest.main()
