# SPDX-License-Identifier: Apache-2.0
"""The release target must not drift from the promised Server minor ranges."""

import copy
import importlib.util
import json
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "check_compatibility", ROOT / "check_compatibility.py"
)
assert SPEC is not None and SPEC.loader is not None
CHECK = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECK)
RECORD = json.loads((ROOT / "compatibility.json").read_text(encoding="utf-8"))


def test_record_agrees_with_target() -> None:
    assert CHECK.server_support_problems(RECORD) == []
    assert CHECK.server_target_constants_problems(RECORD) == []


@pytest.mark.parametrize("lang", ["python", "dotnet", "java", "rust"])
def test_stale_or_missing_target_constant_is_detected(lang: str, tmp_path: Path) -> None:
    for name, (relative, _) in CHECK.TARGET_CONSTANTS.items():
        destination = tmp_path / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        text = (ROOT / relative).read_text(encoding="utf-8")
        if name == lang:
            text = text.replace(RECORD["target_server"], "0.0.0")
        destination.write_text(text, encoding="utf-8")
    assert len(CHECK.server_target_constants_problems(RECORD, tmp_path)) == 1
    (tmp_path / CHECK.TARGET_CONSTANTS[lang][0]).unlink()
    assert len(CHECK.server_target_constants_problems(RECORD, tmp_path)) == 1


@pytest.mark.parametrize("target", [None, "1.1", "latest", "1.1.1-rc.1"])
def test_target_requires_an_exact_release(target: str | None) -> None:
    record = copy.deepcopy(RECORD)
    record["target_server"] = target
    assert CHECK.server_support_problems(record)


@pytest.mark.parametrize("lang", ["python", "dotnet", "java", "rust"])
def test_stale_server_range_is_detected(lang: str) -> None:
    record = copy.deepcopy(RECORD)
    record["clients"][lang]["supported_server"] = ["1.0"]
    assert CHECK.server_support_problems(record)


def test_server_upgrade_does_not_require_matrix_upgrade() -> None:
    record = copy.deepcopy(RECORD)
    record["target_server"] = "1.2.0"
    for lang in ("python", "dotnet", "java", "rust"):
        record["clients"][lang]["supported_server"] = ["1.2", "1.1"]
    assert CHECK.server_support_problems(record) == []
    record["clients"]["matrix-python"]["supported_server"] = ["1.2"]
    assert CHECK.server_support_problems(record)
