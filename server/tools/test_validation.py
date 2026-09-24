# SPDX-License-Identifier: Apache-2.0
"""Offline process controls: python -m unittest discover -s tools -p test_validation.py."""

import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

from check_validation_receipt import validate

TOOLS = Path(__file__).resolve().parent
PWSH = shutil.which("pwsh")


@unittest.skipUnless(PWSH, "PowerShell 7 unavailable")
class RunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.receipt = self.root / "receipt.json"

    def script(self, body):
        # The fixture has one explicit input, not the developer's working tree.
        helper = str(TOOLS / "validation.ps1").replace("'", "''")
        script = self.root / "run.ps1"
        script.write_text(
            f"$ErrorActionPreference = 'Stop'\n. '{helper}'\n"
            "function Get-ValidationSource { param($Root,$Inputs)\n"
            "return @{ commit='fixture'; sha256=(Get-FileHash -LiteralPath (Join-Path $Root 'input.txt')).Hash; inputs=@() } }\n"
            "New-ValidationRun 'fixture' $PSScriptRoot (Join-Path $PSScriptRoot 'receipt.json')\n"
            + body + "\nexit (Invoke-ValidationRun)\n",
            encoding="utf-8",
        )
        (self.root / "input.txt").write_text("fixture", encoding="utf-8")
        return script

    def run_fixture(self, body, code, reason=None):
        result = subprocess.run([PWSH, "-NoProfile", "-File", str(self.script(body))], capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, code, result.stdout + result.stderr)
        receipt = json.loads(self.receipt.read_text(encoding="utf-8-sig"))
        self.assertTrue(receipt["completed"])
        self.assertEqual(receipt["exit_code"], code)
        if reason:
            self.assertEqual(receipt["steps"][0]["reason"], reason)
        return receipt

    def test_success_and_identity_checker(self):
        r = self.run_fixture("Add-ValidationStep 'one' { Invoke-ValidationCommand pwsh @('-NoProfile','-Command','exit 0') } -Requires pwsh", 0, "completed")
        kwargs = dict(run_id=r["run_id"], profile="fixture", source_sha256=r["source_before"]["sha256"], required_steps=["one"])
        self.assertEqual(validate(r, **kwargs), 0)
        for edit in (
            lambda x: x.update(completed=False),
            lambda x: x.update(run_id="stale"),
            lambda x: x.update(required_steps=[]),
            lambda x: x["required_steps"].append("one"),
            lambda x: x["steps"].clear(),
            lambda x: x["steps"][0]["commands"][0].update(exit_code=7),
            lambda x: x["source_after"].update(sha256="other"),
        ):
            broken = copy.deepcopy(r)
            edit(broken)
            with self.assertRaises(ValueError):
                validate(broken, **kwargs)

    def test_native_failure_cannot_be_erased_by_parsing(self):
        r = self.run_fixture("Add-ValidationStep 'one' { Invoke-ValidationCommand pwsh @('-NoProfile','-Command','exit 7'); '{}' | ConvertFrom-Json | Out-Null }", 1, "native_exit")
        self.assertEqual(r["steps"][0]["commands"][0]["exit_code"], 7)

    def test_native_interruption_code_retained(self):
        r = self.run_fixture("Add-ValidationStep 'one' { Invoke-ValidationCommand pwsh @('-NoProfile','-Command','exit 130') }", 1, "native_exit")
        self.assertEqual(r["steps"][0]["commands"][0]["exit_code"], 130)

    def test_exception(self):
        self.run_fixture("Add-ValidationStep 'one' { throw 'do not publish exception secrets' }", 1, "powershell_exception")
        self.assertNotIn("exception secrets", self.receipt.read_text())

    def test_missing_tool(self):
        self.run_fixture("Add-ValidationStep 'one' { throw 'must not execute' } -Requires munarium-nonexistent-fixture-tool", 3, "missing_tool")

    @unittest.skipUnless(shutil.which("git"), "Git unavailable for source identity")
    def test_actual_offline_entrypoint_with_missing_cargo(self):
        env = os.environ.copy()
        env["PATH"] = os.pathsep.join([str(Path(PWSH).parent), str(Path(shutil.which("git")).parent)])
        result = subprocess.run([PWSH, "-NoProfile", "-File", str(TOOLS.parent / "test.ps1"), "-ReceiptPath", str(self.receipt)], env=env, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
        r = json.loads(self.receipt.read_text())
        self.assertTrue(r["completed"])
        self.assertEqual(r["required_steps"], ["tests.workspace", "conformance.memory"])
        self.assertIn("postgres", r["not_requested"])
        self.assertTrue(all(s["outcome"] == "not_run" and s["reason"] == "missing_tool" for s in r["steps"]))

    def test_missing_database(self):
        self.run_fixture("Add-ValidationStep 'one' { throw 'missing_database' }", 3, "missing_database")

    def test_missing_image_is_unavailable(self):
        helper = str(TOOLS / "validation-tiers.ps1").replace("'", "''")
        self.run_fixture(f". '{helper}'; function Invoke-ValidationCommand {{ param($Executable,$Arguments); return }}; Add-ValidationStep 'one' {{ Start-ValidationPostgres }}", 3, "missing_image")

    def test_image_query_failure_is_not_missing_image(self):
        helper = str(TOOLS / "validation-tiers.ps1").replace("'", "''")
        self.run_fixture(f". '{helper}'; function Invoke-ValidationCommand {{ param($Executable,$Arguments); if ($Arguments[0] -eq 'image') {{ throw 'native_exit' }} }}; Add-ValidationStep 'one' {{ Start-ValidationPostgres }}", 1, "native_exit")

    def test_configured_database_failure_and_dependency(self):
        r = self.run_fixture("Add-ValidationStep 'db' { Invoke-ValidationCommand pwsh @('-NoProfile','-Command','exit 2') }; Add-ValidationStep 'query' { throw 'must not execute' } -DependsOn db", 1, "native_exit")
        self.assertEqual(r["steps"][1]["reason"], "dependency_not_passed")
        self.assertEqual(r["steps"][1]["outcome"], "not_run")

    def test_source_mutation(self):
        r = self.run_fixture("Add-ValidationStep 'one' { 'changed' | Set-Content (Join-Path $script:Validation.Root 'input.txt') }", 3)
        self.assertTrue(r["source_changed"])

    def test_environment_restored_on_failure(self):
        self.run_fixture("$env:MUNARIUM_FIXTURE='caller'; Add-ValidationStep 'one' { try { Invoke-ValidationEnvironment @{MUNARIUM_FIXTURE='test'} { throw 'failure' } } catch {}; if ($env:MUNARIUM_FIXTURE -ne 'caller') { throw 'lost caller value' } }", 0)

    def test_environment_absence_reaches_children_and_is_restored(self):
        self.run_fixture(r"""
[Environment]::SetEnvironmentVariable('MUNARIUM_ABSENT_FIXTURE', [NullString]::Value, 'Process')
$env:MUNARIUM_PRESENT_FIXTURE = 'caller'
Add-ValidationStep 'one' {
    Invoke-ValidationEnvironment @{MUNARIUM_ABSENT_FIXTURE='temporary'; MUNARIUM_PRESENT_FIXTURE=$null} {
        Invoke-ValidationCommand pwsh @('-NoProfile','-Command',
            'if (Test-Path Env:MUNARIUM_PRESENT_FIXTURE) { exit 9 }; if ($env:MUNARIUM_ABSENT_FIXTURE -ne "temporary") { exit 8 }')
    }
    if (Test-Path Env:MUNARIUM_ABSENT_FIXTURE) { throw 'absence restored as empty' }
    if ($env:MUNARIUM_PRESENT_FIXTURE -ne 'caller') { throw 'lost caller value' }
    try { Invoke-ValidationEnvironment @{MUNARIUM_ABSENT_FIXTURE='temporary'} { throw 'fixture failure' } } catch {}
    if (Test-Path Env:MUNARIUM_ABSENT_FIXTURE) { throw 'failure restored absence as empty' }
}
""", 0)

    def test_stderr_does_not_corrupt_captured_json(self):
        self.run_fixture("Add-ValidationStep 'one' { $body=Invoke-ValidationCommand pwsh @('-NoProfile','-Command','[Console]::Error.WriteLine(\"diagnostic\"); [Console]::WriteLine(\"{}\")') -Capture; $body | ConvertFrom-Json | Out-Null }", 0)

    def test_argument_redaction(self):
        self.run_fixture("Add-ValidationStep 'one' { $safe=(Protect-ValidationArguments @('--postgres','postgres://user:password@example/db','--token','secret-value','POSTGRES_PASSWORD=another-secret')) -join ' '; if ($safe -match 'user:|secret-value|another-secret') { throw 'secret leak' } }", 0)

    def test_cleanup_failure_is_failure(self):
        r = self.run_fixture("function Clear-ValidationResources { throw 'cleanup failure' }; Add-ValidationStep 'one' { }", 1)
        self.assertEqual(r["reason"], "cleanup_failed")
        self.assertEqual(r["steps"][0]["outcome"], "passed")

    def test_empty_and_invalid_dependency_plans_fail(self):
        for plan in ("", "Add-ValidationStep 'one' {} -DependsOn absent"):
            with self.subTest(plan=plan):
                self.receipt.unlink(missing_ok=True)
                result = subprocess.run([PWSH, "-NoProfile", "-File", str(self.script(plan))], capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                r = json.loads(self.receipt.read_text())
                self.assertFalse(r["completed"])
                self.assertEqual(r["reason"], "runner_error")

    def test_duplicate_steps_rejected_before_execution(self):
        result = subprocess.run([PWSH, "-NoProfile", "-File", str(self.script("Add-ValidationStep 'one' {}; Add-ValidationStep 'one' {}"))], capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 1)
        self.assertFalse(json.loads(self.receipt.read_text())["completed"])

    def test_container_cleanup_uses_only_recorded_id(self):
        self.run_fixture("$script:Validation.Container='owned-fixture-container'; function Invoke-ValidationCommand { param($Executable,$Arguments); if ($Executable -ne 'docker' -or ($Arguments -join ',') -ne 'rm,-f,-v,owned-fixture-container') { throw 'unowned cleanup' } }; Add-ValidationStep 'one' {}", 0)

    def test_semantic_summary_controls(self):
        self.run_fixture("Add-ValidationStep 'good' { Assert-ValidationSummary '{\"passed\":2,\"failed\":0}' }; Add-ValidationStep 'bad' { Assert-ValidationSummary '{\"passed\":2,\"failed\":1}' }; Add-ValidationStep 'empty' { Assert-ValidationSummary '{\"passed\":0,\"failed\":0}' }; Add-ValidationStep 'malformed' { Assert-ValidationSummary 'broken' }", 1)
        r = json.loads(self.receipt.read_text())
        self.assertEqual([s["reason"] for s in r["steps"]], ["completed", "semantic_failure", "malformed_output", "malformed_output"])

    def test_cargo_filter_must_execute_tests(self):
        self.run_fixture("function Invoke-ValidationCommand { 'test result: ok. 0 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out;' }; Add-ValidationStep 'one' { Invoke-ValidationCargoTests @('test','absent-filter') }", 1, "semantic_failure")

    def test_forced_termination_leaves_incomplete_receipt(self):
        script = self.script("Add-ValidationStep 'one' { Start-Sleep -Seconds 120 }")
        process = subprocess.Popen([PWSH, "-NoProfile", "-File", str(script)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            deadline = time.monotonic() + 15
            while time.monotonic() < deadline:
                if self.receipt.exists():
                    r = json.loads(self.receipt.read_text())
                    if r["steps"] and r["steps"][0]["started_at"]:
                        break
                time.sleep(0.05)
            else:
                self.fail("fixture did not start")
            process.kill()
            process.wait(timeout=10)
            r = json.loads(self.receipt.read_text())
            self.assertFalse(r["completed"])
            self.assertEqual(r["steps"][0]["outcome"], "not_run")
            with self.assertRaisesRegex(ValueError, "incomplete_receipt"):
                validate(r, run_id=r["run_id"], profile="fixture", source_sha256=r["source_before"]["sha256"], required_steps=["one"])
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=10)

    def test_cleanup_only_stops_owned_child(self):
        unrelated = subprocess.Popen([PWSH, "-NoProfile", "-Command", "Start-Sleep 120"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            r = self.run_fixture("Add-ValidationStep 'one' { $p=[Diagnostics.ProcessStartInfo]::new(); $p.FileName=(Get-Command pwsh).Source; $p.UseShellExecute=$false; $p.CreateNoWindow=$true; foreach ($a in @('-NoProfile','-Command','Start-Sleep 120')) { $p.ArgumentList.Add($a) }; $child=[Diagnostics.Process]::Start($p); $script:Validation.Processes.Add($child); $script:Validation.Receipt.resources += @{kind='process'; id=$child.Id; cleanup='pending'}; throw 'test failure' }", 1)
            self.assertEqual(r["resources"][0]["cleanup"], "completed")
            self.assertIsNone(unrelated.poll())
        finally:
            unrelated.kill()
            unrelated.wait(timeout=10)


if __name__ == "__main__":
    unittest.main()
