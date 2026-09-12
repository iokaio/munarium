# SPDX-License-Identifier: Apache-2.0
"""Check the root docs gate against small temporary documentation trees."""
import contextlib
import io
import pathlib
import tempfile
import unittest
from unittest.mock import patch

import docs_linkcheck


class DocsLinkTests(unittest.TestCase):
    def check_tree(self, files):
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp).resolve()
            for name, text in files.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text, encoding="utf-8")
            output = io.StringIO()
            with patch.object(docs_linkcheck, "ROOT", root), \
                    patch.object(docs_linkcheck, "DOCS", root / "docs"), \
                    contextlib.redirect_stdout(output):
                status = docs_linkcheck.main()
            return status, output.getvalue()

    def test_missing_image_fails(self):
        status, output = self.check_tree({"docs/README.md": "![Diagram](images/missing.svg)"})
        self.assertEqual(status, 1)
        self.assertIn("broken link -> images/missing.svg", output)

    def test_sibling_index_does_not_cover_page(self):
        status, output = self.check_tree({
            "docs/README.md": "", "docs/a/README.md": "[Page](../b/page.md)",
            "docs/b/README.md": "", "docs/b/page.md": "A page",
        })
        self.assertEqual(status, 1)
        self.assertIn("not listed", output)

    def test_ancestor_index_and_encoded_link_pass(self):
        status, output = self.check_tree({
            "docs/README.md": "[Page](child/a%20page.md#section)",
            "docs/child/README.md": "", "docs/child/a page.md": "A page",
            "docs/example/corpus/document.md": "Unindexed corpus text",
        })
        self.assertEqual(status, 0, output)

    def test_fence_closes_only_with_matching_character_and_length(self):
        text = '\n'.join([
            '````markdown', '```', '[ignored](missing-1.md)', '~~~',
            '[ignored](missing-2.md)', '````', '[checked](real.md)',
        ])
        self.assertEqual(docs_linkcheck.links_in(text), [(7, "real.md")])


if __name__ == "__main__":
    unittest.main()
