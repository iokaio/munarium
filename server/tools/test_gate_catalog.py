# SPDX-License-Identifier: Apache-2.0
"""Independent failure controls for the shared gate execution contract."""

import copy
import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock

import test_validation
from check_gate_equivalence import BASELINE, ROOT, check
from gate_catalog import boundaries, dependency_names, execute, load


class CatalogTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def test_catalog_schema_failures(self):
        data = load()
        for edit in (
            lambda d: d.update(steps=[]),
            lambda d: d["steps"].append(d["steps"][0]),
            lambda d: d["steps"][0].update(required=False),
            lambda d: d["steps"][0].update(depends_on=["absent"]),
            lambda d: d["steps"][0].update(depends_on=[d["steps"][0]["id"]]),
            lambda d: d["steps"][0].update(cwd="../outside"),
            lambda d: d["steps"][0].update(commands=[]),
        ):
            broken = copy.deepcopy(data)
            edit(broken)
            path = self.root / "catalog.json"
            path.write_text(json.dumps(broken), encoding="utf-8")
            with self.assertRaises(ValueError):
                load(path)

    def test_command_failure_stops_before_successful_parser(self):
        runner = Mock(return_value=subprocess.CompletedProcess([], 7))
        with self.assertRaises(RuntimeError):
            execute(
                {
                    "id": "fixture",
                    "cwd": ".",
                    "prerequisites": [],
                    "commands": [["bad"], ["parser"]],
                },
                self.root,
                runner,
            )
        self.assertEqual(runner.call_count, 1)

    def test_working_directory_and_argument_boundaries(self):
        runner = Mock(return_value=subprocess.CompletedProcess([], 0))
        execute(
            {
                "id": "fixture",
                "cwd": "server",
                "prerequisites": [],
                "commands": [["tool", "two words", ";literal"]],
            },
            self.root,
            runner,
        )
        runner.assert_called_once_with(
            ["tool", "two words", ";literal"], cwd=self.root / "server", check=False
        )

    def test_missing_prerequisite_cannot_be_skipped_as_success(self):
        runner = Mock()
        with self.assertRaises(RuntimeError):
            execute(
                {
                    "id": "fixture",
                    "cwd": ".",
                    "prerequisites": ["nonexistent-p16-tool"],
                    "commands": [["tool"]],
                },
                self.root,
                runner,
            )
        runner.assert_not_called()

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell 7 unavailable")
    def test_local_adapter_retains_commands_and_failure(self):
        fixture = test_validation.RunnerTests()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        catalog = self.root / "catalog.json"
        catalog.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "steps": [
                        {
                            "id": "one",
                            "required": True,
                            "cwd": ".",
                            "prerequisites": ["pwsh"],
                            "depends_on": [],
                            "commands": [
                                ["pwsh", "-NoProfile", "-Command", "exit 7"],
                                ["pwsh", "-NoProfile", "-Command", "exit 0"],
                            ],
                        },
                        {
                            "id": "two",
                            "required": True,
                            "cwd": ".",
                            "prerequisites": ["pwsh"],
                            "depends_on": ["one"],
                            "commands": [["pwsh", "-NoProfile", "-Command", "exit 0"]],
                        },
                        {
                            "id": "three",
                            "required": True,
                            "cwd": ".",
                            "prerequisites": ["pwsh"],
                            "depends_on": [],
                            "commands": [["pwsh", "-NoProfile", "-Command", "exit 0"]],
                        },
                    ],
                }
            ),
            encoding="utf-8",
        )
        helper = str(ROOT / "server/tools/gate-catalog.ps1").replace("'", "''")
        catalog_path = str(catalog).replace("'", "''")
        receipt = fixture.run_fixture(
            f". '{helper}'\n$script:GateCatalogPath = '{catalog_path}'\nAdd-CatalogSteps @('one','two','three')",
            1,
            "native_exit",
        )
        one, two, three = receipt["steps"]
        self.assertEqual(len(one["commands"]), 1)
        self.assertEqual(one["commands"][0]["exit_code"], 7)
        self.assertEqual(two["reason"], "dependency_not_passed")
        self.assertEqual(three["outcome"], "passed")

    def tree_runner(self, inject=None):
        def run(argv, **kwargs):
            self.assertTrue(kwargs["check"])
            crate = argv[argv.index("-p") + 1]
            if crate == "munarium-api-types":
                self.assertIn("--all-features", argv)
            output = f"{crate} v1.0.0\nserde v1.0.0\n"
            if inject and inject[0] == crate:
                output += f"{inject[1]} v1.0.0\n"
            return subprocess.CompletedProcess(argv, 0, output)

        return run

    def test_valid_boundaries(self):
        for kind in ("crates", "datastore"):
            boundaries(kind, self.root, self.tree_runner())

    def test_each_boundary_rejects_forbidden_dependency(self):
        # Independent examples for every boundary, including the DTO allowlist.
        for crate, banned in (
            ("munarium-core", "sqlx"),
            ("munarium-access", "axum"),
            ("munarium-providers", "munarium-store-mem"),
            ("munarium-api-types", "munarium-core"),
            ("munarium-datastore", "tonic"),
        ):
            with self.subTest(crate=crate), self.assertRaises(ValueError):
                boundaries(
                    "datastore" if crate == "munarium-datastore" else "crates",
                    self.root,
                    self.tree_runner((crate, banned)),
                )

    def test_failed_resolution_is_not_clean_graph(self):
        runner = Mock(side_effect=subprocess.CalledProcessError(42, ["cargo", "tree"]))
        with self.assertRaises(subprocess.CalledProcessError):
            boundaries("crates", self.root, runner)

    def test_empty_and_malformed_graphs(self):
        for output in ("", "\n", "error: failed", "other v1.0.0\n"):
            with self.assertRaises(ValueError):
                dependency_names(output, "munarium-core")

    def test_retrieval_nested_violation_and_composition_root(self):
        source = self.root / "server/src/munarium-server/src"
        (source / "nested").mkdir(parents=True)
        (source / "state.rs").write_text("use munarium_retrieval_pg::PgRetrieval;")
        nested = source / "nested/handler.rs"
        nested.write_text("use munarium_retrieval::Retrieval;")
        boundaries("retrieval", self.root)
        nested.write_text("use munarium_retrieval_pg::PgRetrieval;")
        with self.assertRaises(ValueError):
            boundaries("retrieval", self.root)

    def test_migration_controls(self):
        source = self.root / "server/src/munarium-store-pg/migrations/nested"
        source.mkdir(parents=True)
        path = source / "001.sql"
        path.write_text("CREATE TABLE example (id BIGINT);")
        boundaries("migrations", self.root)
        for ddl in (
            "DROP TABLE example;",
            "  alter table example drop column id;",
            "DROP COLUMN id;",
        ):
            path.write_text(ddl)
            with self.assertRaises(ValueError):
                boundaries("migrations", self.root)

    def test_missing_source_is_not_success(self):
        for kind in ("retrieval", "migrations"):
            with self.assertRaises(ValueError):
                boundaries(kind, self.root)

    def test_equivalence_positive_and_negative_controls(self):
        baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
        workflow = (ROOT / ".github/workflows/server-ci.yml").read_text(
            encoding="utf-8"
        )
        catalog = load()
        check(workflow, catalog, baseline)
        for edit in (
            lambda c: c["steps"].pop(),
            lambda c: c["steps"][0].update(required=False),
            lambda c: c["steps"][0].update(features="weaker"),
            lambda c: c["steps"][0].update(depends_on=["format"]),
            lambda c: c["steps"][0].update(commands=[["true"]]),
            lambda c: c["boundaries"]["munarium-core"].update(banned=[]),
        ):
            broken = copy.deepcopy(catalog)
            edit(broken)
            with self.assertRaises(ValueError):
                check(workflow, broken, baseline)
        for old, new in (
            ("branches: [main]", "branches: [other]"),
            ("runs-on: ubuntu-latest", "runs-on: other"),
            ("contents: read", "contents: write"),
            ("arguments: --all-features", 'arguments: ""'),
            ("terraform fmt -check -recursive", "true"),
            ("gate_catalog.py clippy.default", "gate_catalog.py format"),
            ('"matrix/contract/**"', '"missing/**"'),
            ('cargo +"$MSRV" test', "cargo test"),
        ):
            self.assertIn(old, workflow)
            with self.assertRaises(ValueError):
                check(workflow.replace(old, new, 1), catalog, baseline)


if __name__ == "__main__":
    unittest.main()
