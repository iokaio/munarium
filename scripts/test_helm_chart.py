# SPDX-License-Identifier: Apache-2.0
"""Rendering checks for the Server Helm chart's replica count (SCALE-1) and
optional builder group (SCALE-2).

Each case renders the chart with `helm template` and inspects the deployment.
They cover the bare chart, the production profile, legacy values (including
keys absent as under `helm upgrade --reuse-values`), an explicit and an
intentionally mismatched integer, invalid values, and the platform-v1 shape.
The builder cases cover the disabled default, a group kept out of every
Service and route, the shared environment and role settings, the replica
count across both groups, the drain bound and staging volumes. When the
`v1.4.0` tag is reachable, legacy values must also render byte for byte as
the v1.4.0 chart renders them.

Limits: this proves what the chart renders, not what a cluster does with it.
Startup guards, effective provider shares, surge, rollback, and adding and
draining a builder with work in flight need a cluster (scale-out.md,
SCALE-1-T1 and SCALE-2-T1). The cases skip when `helm` is not installed.
"""

from __future__ import annotations

import hashlib
import json
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


def documents(*args: str) -> list[str]:
    out = render(*args)
    if out.returncode != 0:
        raise AssertionError(out.stderr)
    return [d for d in out.stdout.split("\n---\n") if "\nkind: " in "\n" + d]


def named(docs: list[str], kind: str, name: str) -> str | None:
    found = [d for d in docs if f"\nkind: {kind}\n" in "\n" + d and f"\n  name: {name}\n" in d]
    if len(found) > 1:
        raise AssertionError(f"{len(found)} {kind}/{name} documents")
    return found[0] if found else None


def env_lines(doc: str) -> list[str]:
    """The container's env entries as rendered text, comments dropped."""
    block = doc.split("\n          env:\n", 1)[1]
    lines = []
    for line in block.splitlines():
        if not line.startswith("            "):
            break
        if line.strip().startswith("#"):
            continue
        lines.append(line.strip())
    return lines


def replica_env(doc: str) -> list[str]:
    return re.findall(r"name: MUNARIUM_REPLICA_COUNT, value: \"([^\"]*)\"", doc)


@unittest.skipUnless(HELM, "helm is not installed; chart rendering unavailable")
class McpPolicyTests(unittest.TestCase):
    def test_both_groups_mount_one_policy_and_roll_on_policy_changes(self):
        base = ["--set", "builder.enabled=true", "--set", "builder.staging.sizeLimit=1Gi"]
        self.assertIsNone(named(documents(*base), "ConfigMap", "munarium-mcp-generation-policy"))
        digests = set()
        for ratio in (3, 4):
            docs = documents(
                *base, "--set", "mcp.generationPolicy.version=1",
                "--set", f"mcp.generationPolicy.model_bytes_per_token.fixture={ratio}"
            )
            config = named(docs, "ConfigMap", "munarium-mcp-generation-policy")
            self.assertIsNotNone(config)
            policy = json.loads(re.search(r"^  policy.json: (.*)$", config, re.M)[1])
            self.assertEqual(json.loads(policy)["model_bytes_per_token"], {"fixture": ratio})
            checksum = hashlib.sha256(policy.encode()).hexdigest()
            digests.add(checksum)
            for app in ("munarium-server", "munarium-builder"):
                deployment_doc = named(docs, "Deployment", app)
                self.assertIn(f'checksum/mcp-generation-policy: "{checksum}"', deployment_doc)
                self.assertIn("name: munarium-mcp-generation-policy", deployment_doc)
                self.assertIn("mountPath: /etc/munarium/mcp-generation, readOnly: true", deployment_doc)
                self.assertIn("value: /etc/munarium/mcp-generation/policy.json", deployment_doc)
            self.assertIn("emptyDir: { sizeLimit:", named(docs, "Deployment", "munarium-builder"))
        self.assertEqual(len(digests), 2)


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


ROLE_ENV = [
    "- { name: MUNARIUM_DATASTORE_BUILDER, value: enabled }",
    "- { name: MUNARIUM_DEPLOYMENT_PLANE, value: builder }",
    "- { name: MUNARIUM_GRPC_ADDR, value: disabled }",
    '- { name: MUNARIUM_SHUTDOWN_GRACE_SECS, value: "20" }',
]
BUILDER = ["--set", "builder.enabled=true"]


@unittest.skipUnless(HELM, "helm is not installed; chart rendering unavailable")
class BuilderGroupTests(unittest.TestCase):
    def test_default_and_legacy_values_render_no_builder(self):
        default = render()
        self.assertEqual(default.returncode, 0, default.stderr)
        self.assertNotIn("munarium-builder", default.stdout)
        self.assertNotIn("MUNARIUM_DATASTORE_BUILDER", default.stdout)
        # Explicitly disabled, absent (as under --reuse-values from v1.4) and
        # a disabled group with other keys set all render the same bytes.
        for args in (
            ["--set", "builder.enabled=false"],
            ["--set", "builder=null"],
            ["--set", "builder.enabled=false", "--set", "builder.replicas=4",
             "--set", "builder.staging.sizeLimit=8Gi"],
        ):
            out = render(*args)
            self.assertEqual(out.returncode, 0, out.stderr)
            self.assertEqual(out.stdout, default.stdout, args)

    def test_builder_group_is_outside_every_service_and_route(self):
        docs = documents(*BUILDER)
        builder = named(docs, "Deployment", "munarium-builder")
        self.assertIsNotNone(builder)
        self.assertIn("selector:\n    matchLabels: { app: munarium-builder }", builder)
        self.assertRegex(builder, r"labels:\n        app: munarium-builder\n")
        serving = named(docs, "Deployment", "munarium-server")
        self.assertIn("matchLabels: { app: munarium-server }", serving)
        services = [d for d in docs if "\nkind: Service\n" in "\n" + d]
        self.assertEqual(len(services), 2)
        for svc in services:
            self.assertIn("selector: { app: munarium-server }", svc)
            self.assertNotIn("munarium-builder", svc)
        routes = [d for d in docs if "Route\n" in d]
        self.assertEqual(len(routes), 2)
        for route in routes:
            self.assertNotIn("munarium-builder", route)

    def test_builder_role_settings_and_shared_environment(self):
        docs = documents(*BUILDER, "--set", "sourceStore.account=acct",
                         "--set", "matrix.baseUrl=http://matrix", "--set", "docIntel.provider=azure")
        builder = env_lines(named(docs, "Deployment", "munarium-builder"))
        serving = env_lines(named(docs, "Deployment", "munarium-server"))
        self.assertEqual(builder[: len(serving)], serving, "the builder shares the serving environment")
        self.assertEqual(builder[len(serving):], ROLE_ENV)
        # The serving group never gains the builder role from the chart.
        for line in ROLE_ENV:
            self.assertNotIn(line, serving)

    def test_enabling_the_builder_leaves_the_serving_group_unchanged(self):
        for extra in ([], ["--set", "providerReplicaCount=4"], ["--set", "replicas=3"]):
            before = named(documents(*extra), "Deployment", "munarium-server")
            after = named(documents(*BUILDER, *extra), "Deployment", "munarium-server")
            self.assertEqual(after, before, extra)

    def test_replicas_resources_and_strategy_are_the_groups_own(self):
        docs = documents(*BUILDER, "--set", "replicas=2", "--set", "builder.replicas=3",
                         "--set", "builder.resources.requests.cpu=2",
                         "--set", "builder.resources.limits.memory=4Gi",
                         "--set", "builder.strategy.type=Recreate")
        builder = named(docs, "Deployment", "munarium-builder")
        self.assertIn("\n  replicas: 3\n", builder)
        self.assertRegex(builder, r"resources:\n\s+limits:\n\s+memory: 4Gi\n\s+requests:\n\s+cpu: 2")
        self.assertRegex(builder, r"strategy:\n\s+type: Recreate")
        serving = named(docs, "Deployment", "munarium-server")
        self.assertIn("\n  replicas: 2\n", serving)
        self.assertNotIn("resources:", serving)
        self.assertNotIn("strategy:", serving)

    def test_auto_counts_every_server_process_once(self):
        def counts(*args: str) -> tuple[list[str], list[str] | None]:
            docs = documents(*args)
            builder = named(docs, "Deployment", "munarium-builder")
            return (replica_env(named(docs, "Deployment", "munarium-server")),
                    replica_env(builder) if builder else None)

        prod = ["-f", str(PRODUCTION)]
        self.assertEqual(counts(*prod, *BUILDER), (["3"], ["3"]))
        self.assertEqual(counts(*prod, *BUILDER, "--set", "builder.replicas=2"), (["4"], ["4"]))
        self.assertEqual(counts(*prod, *BUILDER, "--set", "builder.replicas=0"), (["2"], ["2"]))
        self.assertEqual(counts(*prod, *BUILDER, "--set", "replicas=0", "--set", "builder.replicas=0"),
                         (["1"], ["1"]))
        # A disabled group runs no process and is not counted.
        self.assertEqual(counts(*prod, "--set", "builder.replicas=5"), (["2"], None))
        # A fixed integer is the same divisor on both groups; omission renders none.
        self.assertEqual(counts(*BUILDER, "--set", "providerReplicaCount=7"), (["7"], ["7"]))
        self.assertEqual(counts(*BUILDER), ([], []))

    def test_drain_bound_and_counts_are_validated(self):
        for args, message in (
            (["--set", "builder.shutdownGraceSecs=30"], "builder.shutdownGraceSecs"),
            (["--set", "builder.shutdownGraceSecs=0"], "builder.shutdownGraceSecs"),
            (["--set", "builder.terminationGracePeriodSeconds=10"], "builder.shutdownGraceSecs"),
            (["--set", "builder.replicas=-1"], "builder.replicas"),
            (["--set-string", "builder.replicas=two"], "builder.replicas"),
        ):
            out = render(*BUILDER, *args)
            self.assertNotEqual(out.returncode, 0, args)
            self.assertIn(message, out.stderr, args)
        doc = named(documents(*BUILDER, "--set", "builder.shutdownGraceSecs=50",
                              "--set", "builder.terminationGracePeriodSeconds=60"),
                    "Deployment", "munarium-builder")
        self.assertIn("terminationGracePeriodSeconds: 60", doc)
        self.assertIn('MUNARIUM_SHUTDOWN_GRACE_SECS, value: "50"', doc)

    def test_staging_volumes_render_only_when_sized(self):
        plain = named(documents(*BUILDER), "Deployment", "munarium-builder")
        self.assertNotIn("volumes:", plain)
        self.assertNotIn("MUNARIUM_DATASTORE_STAGING_ROOT", plain)
        sized = named(documents(*BUILDER, "--set", "builder.staging.sizeLimit=64Gi"),
                      "Deployment", "munarium-builder")
        self.assertIn("MUNARIUM_DATASTORE_STAGING_ROOT, value: /var/munarium/staging", sized)
        self.assertIn("- { name: staging, mountPath: /var/munarium/staging }", sized)
        self.assertIn("- { name: scratch, mountPath: /tmp }", sized)
        self.assertEqual(sized.count('emptyDir: { sizeLimit: "64Gi" }'), 2)

    def test_workload_identity_reaches_the_builder(self):
        doc = named(documents(*BUILDER, "--set", "workloadIdentity.clientId=cid",
                              "--set", "sourceStore.account=acct"),
                    "Deployment", "munarium-builder")
        self.assertIn("serviceAccountName: munarium", doc)
        self.assertIn('azure.workload.identity/use: "true"', doc)
        self.assertIn('MUNARIUM_AZURE_CLIENT_ID, value: "cid"', doc)


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
            for extra in (
                pinned,
                [*pinned, "--set", "replicas=3", "--set", "sourceStore.account=acct"],
                [*pinned, "--set", "workloadIdentity.clientId=cid", "--set", "sourceStore.account=acct",
                 "--set", "matrix.baseUrl=http://m", "--set", "matrix.adminUrl=http://a",
                 "--set", "docIntel.provider=azure", "--set", "docIntel.endpoint=http://d",
                 "--set", "metrics.scrapeAnnotations=false", "--set", "gateway.enabled=false",
                 "--set", "directGrpc.enabled=false"],
            ):
                before = render(*extra, chart=old)
                after = render(*extra)
                self.assertEqual(before.returncode, 0, before.stderr)
                self.assertEqual(after.returncode, 0, after.stderr)
                self.assertEqual(after.stdout, before.stdout, extra)


if __name__ == "__main__":
    os.chdir(ROOT)
    unittest.main()
