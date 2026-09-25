# SPDX-License-Identifier: Apache-2.0
"""Negative controls for coverage discovery; no data deletion is performed."""

import copy
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch

import check_retention_inventory as checker


ROOT = Path(__file__).resolve().parents[1]


class RetentionInventoryTests(unittest.TestCase):
    def setUp(self):
        self.inventory = checker.load_json(ROOT / "retention/inventory.json")
        self.artifacts = checker.load_json(ROOT / "retention/artifacts.json")

    def errors(self):
        return checker.validate(ROOT, self.inventory, self.artifacts)

    def changed_schema(self, sql):
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp) / "migrations"
            shutil.copytree(ROOT / "src/munarium-store-pg/migrations", directory)
            (directory / "9999_retention_negative_control.sql").write_text(sql, encoding="utf-8")
            return checker.schema(directory)

    def test_repository_inventory(self):
        self.assertEqual([], self.errors())

    def test_new_table_requires_policy(self):
        actual = self.changed_schema("CREATE TABLE forgotten_copy (id TEXT, payload JSONB);")
        with patch.object(checker, "schema", return_value=actual):
            self.assertIn("table forgotten_copy: missing policy", self.errors())

    def test_new_column_on_existing_table_requires_review(self):
        actual = self.changed_schema("ALTER TABLE sources ADD COLUMN transcript JSONB;")
        with patch.object(checker, "schema", return_value=actual):
            errors = self.errors()
        self.assertTrue(any("table sources: column inventory drift" in error for error in errors))
        self.assertTrue(any("every JSON column" in error for error in errors))

    def test_json_arrays_also_need_payload_notes(self):
        actual = self.changed_schema("ALTER TABLE sources ADD COLUMN transcript JSONB[];")
        self.inventory["tables"]["sources"]["columns"]["transcript"] = "jsonb[]"
        with patch.object(checker, "schema", return_value=actual):
            self.assertIn("table sources: every JSON column needs an explicit payload inventory", self.errors())

    def test_new_artifact_declaration_requires_policy(self):
        self.artifacts["artifacts"]["new_export"] = {
            "description": "A new archive with source text.",
            "origins": ["src/munarium-server/src/answers_api.rs"],
        }
        self.assertIn("artifact new_export: missing policy", self.errors())

    def test_new_component_kind_requires_policy_without_catalog_edit(self):
        model = ROOT / "src/munarium-datastore/src/model.rs"
        with tempfile.TemporaryDirectory() as temp:
            changed = Path(temp) / "model.rs"
            changed.write_text(model.read_text(encoding="utf-8").replace("pub enum ComponentPurpose {", "pub enum ComponentPurpose {\n    Transcript,"), encoding="utf-8")
            kinds = checker.component_kinds(changed)
        with patch.object(checker, "component_kinds", return_value=kinds):
            self.assertIn("artifact datastore_component:Transcript: missing policy", self.errors())

    def test_missing_mode_is_rejected(self):
        del self.inventory["policies"]["sources"]["physical_erasure"]
        self.assertIn("policy sources: must declare all five removal modes", self.errors())

    def test_missing_policy_and_coverage_are_rejected(self):
        self.inventory["tables"]["sources"]["policy"] = "undeclared"
        del self.inventory["policies"]["ledger"]["logical_removal"]["coverage"]
        errors = self.errors()
        self.assertIn("table sources: missing policy undeclared", errors)
        self.assertIn("policy ledger/logical_removal: missing coverage", errors)

    def test_missing_json_inventory_is_rejected(self):
        del self.inventory["tables"]["session_turns"]["json_content"]["completion"]
        self.assertTrue(any("every JSON column" in error for error in self.errors()))

    def test_origin_must_exist_inside_server(self):
        self.inventory["tables"]["sources"]["origins"] = ["../CLAUDE.md"]
        self.assertTrue(any("missing/unsafe origin" in error for error in self.errors()))

    def test_malformed_objects_fail_cleanly(self):
        for path in ((), ("tables",), ("tables", "sources"), ("policies", "sources"), ("policies", "sources", "physical_erasure")):
            with self.subTest(path=path):
                changed = copy.deepcopy(self.inventory)
                if not path:
                    changed = []
                else:
                    parent = changed
                    for key in path[:-1]:
                        parent = parent[key]
                    parent[path[-1]] = []
                with self.assertRaisesRegex(ValueError, "expected an object"):
                    checker.validate(ROOT, changed, self.artifacts)

    def test_artifact_schema_version_is_strict(self):
        for version in (True, "1", 2, None):
            with self.subTest(version=version):
                self.artifacts["version"] = version
                self.assertIn("unsupported registry version", self.errors())

    def test_duplicate_json_keys_are_rejected(self):
        with tempfile.TemporaryDirectory() as temp:
            path = Path(temp) / "duplicate.json"
            path.write_text('{"policies":{},"policies":{}}', encoding="utf-8")
            with self.assertRaisesRegex(ValueError, "duplicate JSON key"):
                checker.load_json(path)

    def test_literals_and_constraints_do_not_become_columns(self):
        actual = self.changed_schema("""
            -- CREATE TABLE comment_copy (payload JSONB);
            CREATE TABLE sample (id TEXT DEFAULT 'CREATE TABLE bogus (x JSONB);',
                metric DOUBLE PRECISION, embedding VECTOR(256),
                payload JSONB DEFAULT '{"key":"a,b"}',
                PRIMARY KEY(id), CHECK(metric IN (1,2)));
            ALTER TABLE sample ADD COLUMN first TEXT, ADD COLUMN second BYTEA;
        """)
        self.assertEqual({"id": "text", "metric": "double precision", "embedding": "vector(256)", "payload": "jsonb", "first": "text", "second": "bytea"}, actual["sample"]["columns"])
        self.assertNotIn("comment_copy", actual)
        self.assertNotIn("bogus", actual)

    def test_unknown_table_ddl_requires_parser_review(self):
        for sql in ("CREATE TABLE new_copy AS SELECT * FROM sources;", "ALTER TABLE sources RENAME COLUMN filename TO renamed;", "CREATE UNLOGGED TABLE cache_copy (body TEXT);", 'CREATE TABLE "quoted_copy" (body TEXT);'):
            with self.subTest(sql=sql), self.assertRaisesRegex(ValueError, "unsupported"):
                self.changed_schema(sql)


if __name__ == "__main__":
    unittest.main()
