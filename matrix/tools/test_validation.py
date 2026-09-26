# SPDX-License-Identifier: Apache-2.0
"""Offline controls for Matrix's receipt adapter; no Docker or service required."""
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PWSH = shutil.which("pwsh")


@unittest.skipUnless(PWSH, "PowerShell 7 unavailable")
class MatrixRunnerTests(unittest.TestCase):
    def run_fixture(self, body, expected):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            script = root / "fixture.ps1"
            shared = str(ROOT / "server/tools/validation.ps1").replace("'", "''")
            adapter = str(ROOT / "matrix/tools/validation-tiers.ps1").replace("'", "''")
            script.write_text(
                f"$ErrorActionPreference='Stop'\n. '{shared}'\n. '{adapter}'\n"
                "function Get-ValidationSource { return @{commit='fixture';sha256='same';inputs=@()} }\n"
                "New-ValidationRun 'matrix.fixture' $PSScriptRoot (Join-Path $PSScriptRoot 'receipt.json')\n"
                + body + "\nexit (Invoke-ValidationRun)\n", encoding="utf-8"
            )
            result = subprocess.run([PWSH, "-NoProfile", "-File", str(script)], capture_output=True, text=True, timeout=45)
            self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
            return json.loads((root / "receipt.json").read_text())

    def test_missing_selected_environment_is_incomplete_not_passed(self):
        receipt = self.run_fixture("""
Add-ValidationStep 'required' {
    Invoke-ValidationEnvironment @{MUNARIUM_MATRIX_TEST_FIXTURE=$null} {
        Assert-MatrixEnvironment @('MUNARIUM_MATRIX_TEST_FIXTURE')
    }
}
Add-ValidationStep 'dependent' { throw 'must not run' } -DependsOn required
""", 3)
        self.assertEqual(receipt["steps"][0]["reason"], "missing_environment")
        self.assertEqual(receipt["steps"][1]["reason"], "dependency_not_passed")

    def summary_fixture(self, text, expected):
        safe = text.replace("'", "''")
        return self.run_fixture(f"""
function Invoke-ValidationCommand {{
    $script:Validation.Current.commands += @{{raw_local_log='fixture.log';exit_code=0;accepted_exit_codes=@(0)}}
    [IO.File]::WriteAllText((Join-Path $script:Validation.Directory 'fixture.log'), '{safe}')
    return '{safe}'
}}
Add-ValidationStep 'tests' {{ Invoke-MatrixTests @('test') }}
""", expected)

    def test_early_return_and_empty_harness_are_not_success(self):
        receipt = self.summary_fixture("SKIPPED: fixture unavailable\ntest result: ok. 1 passed; 0 failed; 0 ignored;", 3)
        self.assertEqual(receipt["steps"][0]["reason"], "missing_environment")
        receipt = self.summary_fixture("test result: ok. 0 passed; 0 failed; 12 ignored;", 1)
        self.assertEqual(receipt["steps"][0]["reason"], "semantic_failure")

    def test_real_summary_retains_ignored_count(self):
        receipt = self.summary_fixture("test result: ok. 12 passed; 0 failed; 3 ignored;", 0)
        self.assertEqual(receipt["steps"][0]["test_summary"], {"passed": 12, "failed": 0, "ignored": 3})

    @unittest.skipUnless(shutil.which("cargo"), "Cargo unavailable for prerequisite resolution")
    def test_registered_closures_keep_filters_and_shared_database(self):
        receipt = self.run_fixture("""
function Invoke-ValidationCommand { param($Executable,$Arguments); if (-not $Arguments.Count) { throw 'lost arguments' } }
function Invoke-MatrixTests {
    param($Arguments)
    if ($Arguments -contains '--ignored') {
        if ($env:MUNARIUM_MATRIX_TEST_DATABASE_URL -ne 'fictional-database') { throw 'lost shared state' }
        $script:Validation.Current['fixture_filter'] = $Arguments[3]
    }
}
function Start-MatrixValidationPostgres { $script:Validation.DatabaseUrl='fictional-database' }
Add-MatrixValidationTiers $true $false $false $false $false
# Stub setup has no Docker prerequisite; this fixture never calls Docker.
($script:Validation.Receipt.steps | Where-Object id -eq 'matrix.postgres.setup').prerequisites=@()
""", 0)
        filters = [s["fixture_filter"] for s in receipt["steps"] if "fixture_filter" in s]
        self.assertEqual(filters, ["scenarios::postgres::", "scenarios::cdc::"])


if __name__ == "__main__":
    unittest.main()
