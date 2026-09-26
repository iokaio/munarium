# SPDX-License-Identifier: Apache-2.0
"""Negative controls for source drift and unpublished installation defaults."""

import json
import tempfile
import unittest
from pathlib import Path

from check_release_versions import INSTALL_DOCS, check

SOURCE = "2.4.0"
PUBLISHED = "2.3.2"


class ReleaseVersionTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.files = {
            "server/Cargo.toml": f'[workspace.package]\nversion = "{SOURCE}"\n',
            "server/release-versions.json": json.dumps({"published_image": PUBLISHED}),
            "server/Dockerfile": f'ARG BUILD_VERSION="{SOURCE}"\n',
            "server/docs/api/openapi.json": json.dumps({"info": {"version": SOURCE}}),
            "clients/server-api.json": json.dumps({"server_version": SOURCE}),
            "clients/compatibility.json": json.dumps({"target_server": SOURCE}),
            "server/deploy/helm/munarium/Chart.yaml": f'appVersion: "{PUBLISHED}"\n',
            "server/deploy/helm/munarium/values.yaml": f'image:\n  # published default\n  repository: ""\n  tag: "{PUBLISHED}"\n',
        }
        for path in INSTALL_DOCS:
            self.files[path] = f"docker pull iokaio/munarium:{PUBLISHED}\n"
        self.files["server/CONTAINER.md"] += (
            f"### Last published image: {PUBLISHED}\n"
            f"docker build --tag iokaio/munarium:{SOURCE}-rc.1 .\n"
        )
        self.write()

    def write(self):
        for name, text in self.files.items():
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")

    def test_preparation_keeps_published_defaults(self):
        self.assertEqual(check(self.root), [])

    def test_publication_moves_all_defaults_together(self):
        self.files = {
            name: text.replace(PUBLISHED, SOURCE) for name, text in self.files.items()
        }
        self.write()
        self.assertEqual(check(self.root), [])

    def test_each_source_version_drift_fails(self):
        for name in (
            "server/Cargo.toml",
            "server/Dockerfile",
            "server/docs/api/openapi.json",
            "clients/server-api.json",
            "clients/compatibility.json",
        ):
            with self.subTest(path=name):
                original = self.files[name]
                self.files[name] = original.replace(SOURCE, "2.2.0")
                self.write()
                self.assertTrue(check(self.root))
                self.files[name] = original
                self.write()

    def test_each_published_default_drift_fails(self):
        for name in (
            "server/deploy/helm/munarium/Chart.yaml",
            "server/deploy/helm/munarium/values.yaml",
            *INSTALL_DOCS,
        ):
            with self.subTest(path=name):
                original = self.files[name]
                self.files[name] = original.replace(PUBLISHED, SOURCE)
                self.write()
                self.assertTrue(any(name in error for error in check(self.root)))
                self.files[name] = original
                self.write()

    def test_helm_pin_is_checked(self):
        self.files["server/deploy/helm/munarium/README.md"] = (
            f"helm install --set image.tag={SOURCE}\n"
        )
        self.write()
        self.assertTrue(any("README.md" in error for error in check(self.root)))

    def test_unpublished_or_mutable_install_tag_fails(self):
        for tag in (f"{SOURCE}-rc.1", "latest", "2.4"):
            with self.subTest(tag=tag):
                self.files["README.md"] = f"docker pull iokaio/munarium:{tag}\n"
                self.write()
                self.assertTrue(any("README.md" in error for error in check(self.root)))

    def test_missing_or_duplicate_declarations_fail(self):
        for value in ("", f'ARG BUILD_VERSION="{SOURCE}"\n' * 2):
            with self.subTest(value=value):
                self.files["server/Dockerfile"] = value
                self.write()
                self.assertTrue(
                    any("Dockerfile" in error for error in check(self.root))
                )

    def test_missing_input_fails(self):
        (self.root / "server/release-versions.json").unlink()
        self.assertTrue(check(self.root))

    def test_malformed_version_fails(self):
        for version in ("latest", "2.4", "2.4.0-rc.1", 123):
            with self.subTest(version=version):
                self.files["server/release-versions.json"] = json.dumps(
                    {"published_image": version}
                )
                self.write()
                self.assertTrue(check(self.root))

    def test_published_version_cannot_exceed_source(self):
        self.files = {
            name: text.replace(PUBLISHED, "3.0.0") for name, text in self.files.items()
        }
        self.write()
        self.assertIn(
            "server/release-versions.json: published image is newer than source",
            check(self.root),
        )


if __name__ == "__main__":
    unittest.main()
