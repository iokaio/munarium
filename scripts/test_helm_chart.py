# SPDX-License-Identifier: Apache-2.0
"""Rendering checks for the Server Helm chart's replica count (SCALE-1).

Each case renders the chart with `helm template` and inspects the deployment.
They cover the bare chart, the production profile, legacy values (including
keys absent as under `helm upgrade --reuse-values`), an explicit and an
intentionally mismatched integer, invalid values, and the platform-v1 shape.
When the `v1.4.0` tag is reachable, legacy values must also render byte for
byte as the v1.4.0 chart renders them.

Limits: this proves what the chart renders, not what a cluster does with it.
Startup guards, effective provider shares, surge and rollback need a cluster
(scale-out.md, SCALE-1-T1). The cases skip when `helm` is not installed.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from io import BytesIO
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CHART = ROOT / "server/deploy/helm/munarium"
PRODUCTION = CHART / "values-production.yaml"
HELM = shutil.which("helm")
BASE = ["--set", "image.repository=example.invalid/munarium"]


def render(*args: str, chart: Path = CHART) -> subprocess.CompletedProcess:
    return subprocess.run(
        [HELM, "template", "munarium", str(chart), "--namespace", "munarium", *BASE, *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )


def deployment(*args: str) -> str:
    out = render(*args, "--show-only", "templates/deployment.yaml")
    if out.returncode != 0:
        raise AssertionError(out.stderr)
    # Helm orders documents by kind, so find the Deployment by its kind.
    docs = [d for d in out.stdout.split("\n---\n") if "\nkind: Deployment\n" in d]
    if len(docs) != 1:
        raise AssertionError(f"expected one Deployment, got {len(docs)}:\n{out.stdout}")
    return docs[0]


def replica_env(doc: str) -> list[str]:
    return re.findall(r"name: MUNARIUM_REPLICA_COUNT, value: \"([^\"]*)\"", doc)


@unittest.skipUnless(HELM, "helm is not installed; chart rendering unavailable")
class ProviderReplicaCountTests(unittest.TestCase):
    def test_bare_chart_keeps_the_v14_omission(self):
        doc = deployment()
        self.assertIn("replicas: 2", doc)
        self.assertEqual(replica_env(doc), [])
        self.assertNotIn("strategy:", doc)

    def test_keys_absent_under_reuse_values_render_as_omitted(self):
        # `--reuse-values` from a v1.4 release carries the v1.4 chart's values,
        # which have neither key. Null deletes the new defaults the same way.
        doc = deployment("--set", "providerReplicaCount=null", "--set", "strategy=null")
        self.assertEqual(replica_env(doc), [])
        self.assertNotIn("strategy:", doc)
        doc = deployment("--set-string", "providerReplicaCount=")
        self.assertEqual(replica_env(doc), [])

    def test_production_profile_renders_the_serving_count(self):
        self.assertEqual(replica_env(deployment("-f", str(PRODUCTION))), ["2"])
        self.assertEqual(replica_env(deployment("-f", str(PRODUCTION), "--set", "replicas=3")), ["3"])
        self.assertEqual(replica_env(deployment("-f", str(PRODUCTION), "--set", "replicas=0")), ["1"])

    def test_explicit_and_mismatched_integers_render_verbatim(self):
        self.assertEqual(replica_env(deployment("--set", "providerReplicaCount=2")), ["2"])
        mismatch = deployment("--set", "providerReplicaCount=4", "--set", "replicas=3")
        self.assertIn("replicas: 3", mismatch)
        self.assertEqual(replica_env(mismatch), ["4"])
        self.assertEqual(replica_env(deployment("--set-string", "providerReplicaCount=3")), ["3"])
        with tempfile.TemporaryDirectory() as tmp:
            values = Path(tmp) / "values.yaml"
            values.write_text("providerReplicaCount: 5\n", encoding="utf-8")
            self.assertEqual(replica_env(deployment("-f", str(values))), ["5"])

    def test_invalid_counts_fail_the_render(self):
        for args in (
            ["--set", "providerReplicaCount=0"],
            ["--set", "providerReplicaCount=-1"],
            ["--set", "providerReplicaCount=1.5"],
            ["--set", "providerReplicaCount=true"],
            ["--set-string", "providerReplicaCount=two"],
            ["--set-string", "providerReplicaCount=Auto"],
        ):
            out = render(*args)
            self.assertNotEqual(out.returncode, 0, args)
            self.assertIn("providerReplicaCount must be omitted", out.stderr, args)

    def test_platform_v1_shape_is_one_process_without_overlap(self):
        doc = deployment(
            "--set", "replicas=1",
            "--set", "providerReplicaCount=1",
            "--set", "strategy.type=Recreate",
        )
        self.assertIn("replicas: 1", doc)
        self.assertEqual(replica_env(doc), ["1"])
        self.assertRegex(doc, r"strategy:\n\s+type: Recreate")

    def test_bounded_surge_renders_verbatim(self):
        doc = deployment(
            "-f", str(PRODUCTION),
            "--set", "strategy.rollingUpdate.maxSurge=0",
            "--set", "strategy.rollingUpdate.maxUnavailable=1",
        )
        self.assertRegex(doc, r"strategy:\n\s+rollingUpdate:\n\s+maxSurge: 0\n\s+maxUnavailable: 1")


def v14_chart(dest: Path) -> Path | None:
    """Extract the chart at v1.4.0, or None when the tag is unavailable."""
    git = shutil.which("git")
    if not git:
        return None
    out = subprocess.run(
        [git, "-C", str(ROOT), "archive", "--format=tar", "v1.4.0", "server/deploy/helm/munarium"],
        capture_output=True,
        check=False,
    )
    if out.returncode != 0:
        return None
    with tarfile.open(fileobj=BytesIO(out.stdout)) as tar:
        tar.extractall(dest, filter="data")
    return dest / "server/deploy/helm/munarium"


@unittest.skipUnless(HELM, "helm is not installed; chart rendering unavailable")
class LegacyUpgradeTests(unittest.TestCase):
    def test_v14_values_render_exactly_as_the_v14_chart(self):
        with tempfile.TemporaryDirectory() as tmp:
            old = v14_chart(Path(tmp))
            if old is None:
                self.skipTest("tag v1.4.0 is not reachable in this checkout")
            # The v1.4.0 tag predates the published-image default, so pin the tag.
            pinned = ["--set", "image.tag=1.4.0"]
            for extra in (pinned, [*pinned, "--set", "replicas=3", "--set", "sourceStore.account=acct"]):
                before = render(*extra, chart=old)
                after = render(*extra)
                self.assertEqual(before.returncode, 0, before.stderr)
                self.assertEqual(after.returncode, 0, after.stderr)
                self.assertEqual(after.stdout, before.stdout, extra)


if __name__ == "__main__":
    os.chdir(ROOT)
    unittest.main()
