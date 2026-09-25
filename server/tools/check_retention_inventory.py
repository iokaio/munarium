# SPDX-License-Identifier: Apache-2.0
"""Check declared retention coverage, not runtime erasure or arbitrary data flow.

Only the repository's additive CREATE TABLE / ALTER TABLE ADD COLUMN dialect is
accepted. Unknown table DDL requires deliberate parser and inventory review.
Non-SQL surfaces must be declared in retention/artifacts.json; datastore
ComponentPurpose variants are independently discovered from the Rust model.
"""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path


MODES = {
    "retrieval_exclusion",
    "access_revocation",
    "logical_removal",
    "physical_erasure",
    "protected_retention",
}
FIELDS = {"read", "cleanup", "holds", "restore", "coverage"}
IDENT = r"[a-z_][a-z_0-9]*"
IGNORED_SQL = re.compile(
    r"--[^\n]*|/\*[\s\S]*?\*/|'(?:''|[^'])*'|\$(?P<tag>[a-z_0-9]*)\$[\s\S]*?\$(?P=tag)\$",
    re.IGNORECASE,
)


def sql_code(text: str) -> str:
    """Mask comments and literals, preserving offsets and statement boundaries."""
    return IGNORED_SQL.sub(lambda m: re.sub(r"[^\n]", " ", m.group()), text)


def split_fields(text: str) -> list[str]:
    fields, start, depth = [], 0, 0
    for offset, char in enumerate(text):
        if char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
        elif char == "," and depth == 0:
            fields.append(text[start:offset].strip())
            start = offset + 1
        if depth < 0:
            raise ValueError("unbalanced SQL parentheses")
    if depth:
        raise ValueError("unbalanced SQL parentheses")
    fields.append(text[start:].strip())
    return fields


def column(field: str) -> tuple[str, str]:
    match = re.match(rf"^({IDENT})\s+(DOUBLE\s+PRECISION|{IDENT}(?:\s*\([^)]*\))?(?:\[\])*)", field, re.I)
    if not match:
        raise ValueError(f"unsupported column declaration: {field[:80]}")
    rest = field[match.end():].strip()
    if rest and not re.match(r"(?:NOT|NULL|PRIMARY|REFERENCES|UNIQUE|CHECK|GENERATED|DEFAULT|COLLATE|CONSTRAINT)\b", rest, re.I):
        raise ValueError(f"unsupported column type/modifier: {field[:80]}")
    return match[1].lower(), re.sub(r"\s+", " ", match[2]).lower()


def schema(migrations: Path) -> dict[str, dict]:
    result: dict[str, dict] = {}
    for migration in sorted(migrations.glob("*.sql")):
        for statement in sql_code(migration.read_text(encoding="utf-8-sig")).split(";"):
            statement = statement.strip()
            if re.match(r"CREATE\s+TABLE\b", statement, re.I):
                match = re.match(rf"CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?({IDENT})\s+([\s\S]+)", statement, re.I)
                if not match:
                    raise ValueError(f"{migration.name}: unsupported CREATE TABLE")
                name, definition = match[1].lower(), match[2].strip()
                if name in result:
                    raise ValueError(f"{migration.name}: duplicate CREATE TABLE {name}")
                parent = re.match(rf"PARTITION\s+OF\s+({IDENT})\b", definition, re.I)
                if parent:
                    if parent[1].lower() not in result:
                        raise ValueError(f"unknown partition parent {parent[1]}")
                    result[name] = {"columns": dict(result[parent[1].lower()]["columns"]), "origin": migration.name, "parent": parent[1].lower()}
                    continue
                if not definition.startswith("("):
                    raise ValueError(f"{migration.name}: unsupported CREATE TABLE {name}")
                depth, end = 0, None
                for offset, char in enumerate(definition):
                    depth += (char == "(") - (char == ")")
                    if depth == 0:
                        end = offset
                        break
                if end is None:
                    raise ValueError(f"{migration.name}: unclosed CREATE TABLE {name}")
                columns = {}
                for field in split_fields(definition[1:end]):
                    if re.match(r"(?:PRIMARY|FOREIGN|UNIQUE|CHECK|CONSTRAINT|EXCLUDE)\b", field, re.I):
                        continue
                    key, dtype = column(field)
                    if key in columns:
                        raise ValueError(f"duplicate column {name}.{key}")
                    columns[key] = dtype
                result[name] = {"columns": columns, "origin": migration.name}
            elif re.match(r"ALTER\s+TABLE\b", statement, re.I):
                match = re.match(rf"ALTER\s+TABLE\s+({IDENT})\s+([\s\S]+)", statement, re.I)
                if not match or match[1].lower() not in result:
                    raise ValueError(f"{migration.name}: unsupported ALTER TABLE")
                table = match[1].lower()
                for field in split_fields(match[2]):
                    add = re.match(r"ADD\s+COLUMN\s+(?:IF\s+NOT\s+EXISTS\s+)?([\s\S]+)", field, re.I)
                    if not add:
                        raise ValueError(f"{migration.name}: unsupported ALTER TABLE {table}: {field[:80]}")
                    key, dtype = column(add[1])
                    result[table]["columns"][key] = dtype
                    for child in result.values():
                        if child.get("parent") == table:
                            child["columns"][key] = dtype
            elif re.match(r"(?:DROP\s+TABLE|CREATE\s+(?:UNLOGGED|TEMP(?:ORARY)?)\s+TABLE)\b", statement, re.I):
                raise ValueError(f"{migration.name}: unsupported table DDL")
    if not result:
        raise ValueError("no schema tables found")
    return result


def component_kinds(model: Path) -> set[str]:
    text = re.sub(r"//[^\n]*", "", model.read_text(encoding="utf-8"))
    match = re.search(r"pub enum ComponentPurpose\s*\{([^}]+)\}", text)
    if not match:
        raise ValueError("ComponentPurpose declaration missing")
    kinds = set()
    for variant in match[1].split(","):
        variant = variant.strip()
        if variant:
            if not re.fullmatch(r"[A-Z][A-Za-z0-9]+", variant):
                raise ValueError("ComponentPurpose syntax changed; review retention discovery")
            kinds.add("datastore_component:" + variant)
    return kinds


def load_json(path: Path) -> dict:
    def unique(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError(f"{path.name}: duplicate JSON key {key}")
            value[key] = item
        return value

    return json.loads(path.read_text(encoding="utf-8"), object_pairs_hook=unique)


def validate(root: Path, inventory: dict | None = None, artifacts: dict | None = None) -> list[str]:
    inventory = inventory if inventory is not None else load_json(root / "retention/inventory.json")
    artifacts = artifacts if artifacts is not None else load_json(root / "retention/artifacts.json")
    def object_value(value, label):
        if not isinstance(value, dict):
            raise ValueError(f"{label}: expected an object")
        return value

    object_value(inventory, "inventory")
    object_value(artifacts, "artifact declarations")
    actual = schema(root / "src/munarium-store-pg/migrations")
    errors: list[str] = []
    profiles = object_value(inventory.get("policies"), "policies")
    tables = object_value(inventory.get("tables"), "tables")
    declared_artifacts = object_value(artifacts.get("artifacts"), "artifact declarations")
    policies_artifacts = object_value(inventory.get("artifacts"), "artifact policies")

    def text_fields(value: dict, fields: set[str], label: str):
        object_value(value, label)
        for field in sorted(fields):
            if not isinstance(value.get(field), str) or not value[field].strip():
                errors.append(f"{label}: missing {field}")

    def paths(paths: list[str], label: str):
        if not isinstance(paths, list) or not paths:
            errors.append(f"{label}: missing origins")
            return
        for path in paths:
            if not isinstance(path, str):
                errors.append(f"{label}: origin must be a path string")
                continue
            target = (root / path).resolve()
            if not target.is_relative_to(root.resolve()) or not target.is_file():
                errors.append(f"{label}: missing/unsafe origin {path}")

    if any(type(doc.get("version")) is not int or doc["version"] != 1 for doc in (inventory, artifacts)):
        errors.append("unsupported registry version")
    for name, profile in profiles.items():
        object_value(profile, "policy " + name)
        if set(profile) != MODES:
            errors.append(f"policy {name}: must declare all five removal modes")
        for mode, policy in profile.items():
            text_fields(policy, FIELDS, f"policy {name}/{mode}")

    def surface(name: str, item: dict):
        text_fields(item, {"owner", "content", "policy"}, name)
        if not isinstance(item.get("policy"), str) or item["policy"] not in profiles:
            errors.append(f"{name}: missing policy {item.get('policy')}")
        paths(item.get("origins"), name)

    for name in sorted(set(actual) | set(tables)):
        if name not in tables:
            errors.append(f"table {name}: missing policy")
            continue
        if name not in actual:
            errors.append(f"table {name}: absent from migrations")
            continue
        item = tables[name]
        surface("table " + name, item)
        object_value(item.get("columns"), "table " + name + " columns")
        columns = actual[name]["columns"]
        if item.get("columns") != columns:
            errors.append(f"table {name}: column inventory drift (missing={sorted(set(columns) - set(item.get('columns', {})))}, extra={sorted(set(item.get('columns', {})) - set(columns))}; review names and types")
        json_columns = {key for key, dtype in columns.items() if re.fullmatch(r"jsonb?(?:\[\])*", dtype)}
        payloads = object_value(item.get("json_content"), "table " + name + " JSON")
        if set(payloads) != json_columns:
            errors.append(f"table {name}: every JSON column needs an explicit payload inventory")
        text_fields(payloads, json_columns, "table " + name + " JSON")

    expected_artifacts = set(declared_artifacts) | component_kinds(root / "src/munarium-datastore/src/model.rs")
    for name in sorted(expected_artifacts | set(policies_artifacts)):
        if name not in policies_artifacts:
            errors.append(f"artifact {name}: missing policy")
        elif name not in expected_artifacts:
            errors.append(f"artifact {name}: lacks declaration")
        else:
            surface("artifact " + name, policies_artifacts[name])
    for name, declaration in declared_artifacts.items():
        text_fields(declaration, {"description"}, "artifact declaration " + name)
        paths(declaration.get("origins"), "artifact declaration " + name)
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server-root", type=Path, default=Path(__file__).resolve().parents[1])
    args = parser.parse_args()
    try:
        errors = validate(args.server_root)
    except (ValueError, OSError, TypeError, KeyError) as error:
        errors = [str(error)]
    if errors:
        print("Retention inventory failed:\n" + "\n".join("  " + error for error in errors))
        return 1
    print("Retention inventory covers migration tables/columns, declared artifacts, and datastore component kinds.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
