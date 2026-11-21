# SPDX-License-Identifier: Apache-2.0
"""Negative controls for the LENS-D2 Git inventory."""

import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import lens_workspace as workspace


class WorkspaceTests(unittest.TestCase):
    def test_tree_uses_nul_boundaries_and_distinguishes_gitlinks(self):
        oid = "a" * 40
        data = (f"100644 blob {oid} 5\tspace and\tnew\nline.md\0"
                f"160000 commit {oid} -\tother\0").encode()
        doc, link = workspace.tree_entries(data)
        self.assertEqual(doc["path"], "space and\tnew\nline.md")
        self.assertEqual(workspace.classify(doc, set()), ("docs", "documentation"))
        self.assertIsNone(link["size"])
        self.assertEqual(workspace.classify(link, set()), ("review", "gitlink"))
        with self.assertRaises(ValueError):
            workspace.tree_entries(data[:-1])
        with self.assertRaises(ValueError):
            workspace.tree_entries(data + data)

    def test_symlink_pointer_and_unknown_file_stay_out_of_corpus(self):
        entry = {"mode": "120000", "type": "blob", "oid": "a" * 40, "path": "link.md"}
        self.assertEqual(workspace.classify(entry, set()), ("excluded", "symlink"))
        entry["mode"] = "100644"
        self.assertEqual(workspace.classify(entry, {entry["oid"]}), ("review", "large_file_pointer"))
        entry["path"] = "input.unknown"
        self.assertEqual(workspace.classify(entry, set()), ("review", "unclassified_path"))
        entry["path"] = "scripts/i3_pilot_fixture.json"
        self.assertEqual(workspace.classify(entry, set()), ("excluded", "evaluation_material"))

    def test_restricted_root_fails_before_git_without_authority(self):
        with patch.object(workspace, "git") as git, tempfile.TemporaryDirectory() as directory:
            with self.assertRaisesRegex(ValueError, "explicit corpus authority"):
                workspace.freeze(Path(directory), {workspace.RESTRICTED: "a" * 40}, None)
            git.assert_not_called()


if __name__ == "__main__":
    unittest.main()
