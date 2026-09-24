# SPDX-License-Identifier: Apache-2.0
# Shared local execution/receipt contract. Dot-source; no work runs on import.
#Requires -Version 7
function Get-ValidationSource {
    param([string]$Root, [string[]]$Inputs)
    $head = & git -C $Root rev-parse HEAD 2>$null
    if ($LASTEXITCODE -ne 0) { throw 'source_identity_unavailable' }
    $tracked = & git -C $Root -c core.quotepath=false ls-files 2>$null
    if ($LASTEXITCODE -ne 0) { throw 'source_manifest_unavailable' }
    $entries = @(foreach ($path in @($tracked + $Inputs | Sort-Object -Unique)) {
        $full = [IO.Path]::GetFullPath((Join-Path $Root $path))
        if (-not $full.StartsWith($Root.TrimEnd('/','\') + [IO.Path]::DirectorySeparatorChar)) { throw 'input_outside_repository' }
        if ($path -match '(^|/)(scratch|target|\.git)/|(^|/)\.env$|\.local\.toml$') { throw 'unsafe_input' }
        $digest = if (Test-Path -LiteralPath $full -PathType Leaf) { (Get-FileHash -LiteralPath $full -Algorithm SHA256).Hash.ToLowerInvariant() } else { 'missing' }
        [ordered]@{ path = $path; sha256 = $digest }
    })
    $bytes = [Text.Encoding]::UTF8.GetBytes(($entries | ConvertTo-Json -Compress))
    [ordered]@{ commit = "$head"; sha256 = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)).ToLowerInvariant(); inputs = $entries }
}

function Save-ValidationReceipt {
    $json = $script:Validation.Receipt | ConvertTo-Json -Depth 30
    $tmp = $script:Validation.Path + '.tmp'
    [IO.File]::WriteAllText($tmp, $json + "`n")
    [IO.File]::Move($tmp, $script:Validation.Path, $true)
}

function New-ValidationRun {
    param([string]$Profile, [string]$Root, [string]$ReceiptPath, [string[]]$Inputs = @(), [string[]]$NotRequested = @())
    $id = [guid]::NewGuid().ToString('N')
    # Explicit safe additions for an uncommitted checkout; never discover
    # another contributor's untracked inputs by walking the filesystem.
    $Inputs += @(
        'server/tools/validation.ps1', 'server/tools/validation-tiers.ps1',
        'server/tools/test-json-features.ps1', 'server/tools/test_validation.py',
        'server/tools/check_validation_receipt.py', 'server/docs/guides/validation.md',
        'server/src/munarium-server/src/json_persistence_tests.rs'
    )
    $dir = Join-Path $Root "server/scratch/validation/$id"
    [void][IO.Directory]::CreateDirectory($dir)
    if (-not $ReceiptPath) { $ReceiptPath = Join-Path $dir 'receipt.json' }
    $ReceiptPath = [IO.Path]::GetFullPath($ReceiptPath)
    if (Test-Path -LiteralPath $ReceiptPath) { throw 'receipt_already_exists' }
    [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($ReceiptPath))
    $script:Validation = @{
        Path = $ReceiptPath; Directory = $dir; Root = $Root; Inputs = $Inputs
        Bodies = @{}; Current = $null; Processes = [Collections.Generic.List[object]]::new()
        Container = $null; DatabaseUrl = $null
        Receipt = [ordered]@{
            schema_version = 1; run_id = $id; profile = $Profile
            started_at = [DateTime]::UtcNow.ToString('o'); ended_at = $null; duration_ms = $null
            completed = $false; exit_code = $null; reason = 'incomplete'; source_changed = $null
            source_before = $null; source_after = $null
            tools = [ordered]@{ powershell = "$($PSVersionTable.PSVersion)" }
            required_steps = @(); steps = @(); not_requested = @($NotRequested | Where-Object { $null -ne $_ }); waivers = @(); resources = @()
        }
    }
    Save-ValidationReceipt
}

function Add-ValidationStep {
    param([string]$Id, [scriptblock]$Body, [string[]]$Requires = @(), [string[]]$DependsOn = @())
    if (-not $Id -or $script:Validation.Bodies.ContainsKey($Id)) { throw 'duplicate_or_empty_step_id' }
    $script:Validation.Bodies[$Id] = $Body
    $script:Validation.Receipt.required_steps += $Id
    $script:Validation.Receipt.steps += [ordered]@{
        id = $Id; outcome = 'not_run'; reason = 'pending'; prerequisites = @($Requires | Where-Object { $null -ne $_ })
        prerequisite_resolution = @(); depends_on = @($DependsOn | Where-Object { $null -ne $_ })
        commands = @(); started_at = $null; ended_at = $null; duration_ms = $null
    }
    Save-ValidationReceipt
}

function Protect-ValidationArguments {
    param([string[]]$Arguments)
    $hideNext = $false
    foreach ($arg in $Arguments) {
        if ($hideNext) { '<redacted>'; $hideNext = $false; continue }
        if ($arg -match '^--(postgres|token|rw-token|mgmt-token|password)$') { $hideNext = $true; $arg; continue }
        if ($arg -match '(?i)(postgres(ql)?://|password=|secret=|token=)') { '<redacted>'; continue }
        $arg
    }
}

function Invoke-ValidationCommand {
    param([string]$Executable, [string[]]$Arguments = @(), [switch]$Capture, [int[]]$AcceptExitCodes = @(0))
    $command = Get-Command $Executable -ErrorAction SilentlyContinue
    if (-not $command) { throw 'missing_tool' }
    $record = [ordered]@{ executable = $command.Source; arguments = @(Protect-ValidationArguments $Arguments); exit_code = $null; accepted_exit_codes = @($AcceptExitCodes) }
    if ($script:Validation.Current) { $script:Validation.Current.commands += $record; Save-ValidationReceipt }
    $script:Validation.Receipt.tools[$command.Source] = @{ sha256 = (Get-FileHash -LiteralPath $command.Source).Hash }
    $psi = [Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $command.Source; $psi.WorkingDirectory = (Get-Location).Path
    $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true; $psi.RedirectStandardError = $true
    foreach ($arg in $Arguments) { $psi.ArgumentList.Add($arg) }
    $process = [Diagnostics.Process]::Start($psi)
    $script:Validation.Processes.Add($process)
    $record['process_id'] = $process.Id
    $record['started_at'] = $process.StartTime.ToUniversalTime().ToString('o')
    if ($script:Validation.Current) { Save-ValidationReceipt }
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    while (-not $process.WaitForExit(500)) { }
    # Capture native status before parsing. Stdout and stderr stay separate so
    # compiler diagnostics cannot become part of generated JSON/documents.
    $code = $process.ExitCode
    $result = $stdout.GetAwaiter().GetResult()
    $errors = $stderr.GetAwaiter().GetResult()
    $record.exit_code = $code
    $logName = 'command-' + [guid]::NewGuid().ToString('N') + '.log'
    [IO.File]::WriteAllText((Join-Path $script:Validation.Directory $logName), $result + "`n" + $errors)
    $record['raw_local_log'] = $logName
    if ($script:Validation.Current) { Save-ValidationReceipt }
    if ($code -notin $AcceptExitCodes) { throw 'native_exit' }
    if ($Capture) { return ($result -split '\r?\n' | Where-Object { $_ -ne '' }) }
    Write-Host $result
}

function Invoke-ValidationEnvironment {
    param([hashtable]$Values, [scriptblock]$Body)
    $old = @{}
    try {
        foreach ($key in $Values.Keys) {
            $old[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
            # PowerShell binds ordinary $null to an empty System.String. Newer
            # .NET preserves empty environment values, so pass an actual null
            # string to remove the variable (absence is not an empty DB URL).
            if ($null -eq $Values[$key]) {
                [Environment]::SetEnvironmentVariable($key, [NullString]::Value, 'Process')
            } else {
                [Environment]::SetEnvironmentVariable($key, $Values[$key], 'Process')
            }
        }
        & $Body
    } finally {
        foreach ($key in $old.Keys) {
            if ($null -eq $old[$key]) {
                [Environment]::SetEnvironmentVariable($key, [NullString]::Value, 'Process')
            } else {
                [Environment]::SetEnvironmentVariable($key, $old[$key], 'Process')
            }
        }
    }
}

function Invoke-ValidationCargoTests {
    param([string[]]$Arguments)
    $output = (Invoke-ValidationCommand cargo $Arguments -Capture) -join "`n"
    $summaries = [regex]::Matches($output, '(?m)^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;')
    if ($summaries.Count -eq 0) { throw 'malformed_output' }
    $passed = 0L; $failed = 0L; $ignored = 0L
    foreach ($summary in $summaries) {
        $passed += [long]$summary.Groups[1].Value
        $failed += [long]$summary.Groups[2].Value
        $ignored += [long]$summary.Groups[3].Value
    }
    if ($passed -eq 0 -or $failed -ne 0) { throw 'semantic_failure' }
    $script:Validation.Current['test_summary'] = @{ passed = $passed; failed = $failed; ignored = $ignored }
    Write-Host "Cargo: $passed passed; $failed failed; $ignored ignored (not executed)."
}

function Invoke-ValidationRun {
    $r = $script:Validation.Receipt
    $finished = $false
    try {
        if ($r.steps.Count -eq 0) { throw 'empty_profile' }
        foreach ($step in $r.steps) {
            foreach ($dep in $step.depends_on) {
                if ($dep -notin $r.required_steps -or [array]::IndexOf($r.required_steps, $dep) -ge [array]::IndexOf($r.required_steps, $step.id)) { throw 'invalid_dependency' }
            }
        }
        $r.source_before = Get-ValidationSource $script:Validation.Root $script:Validation.Inputs
        Save-ValidationReceipt
        foreach ($step in $r.steps) {
            $script:Validation.Current = $step
            if (@($r.steps | Where-Object { $_.id -in $step.depends_on -and $_.outcome -ne 'passed' }).Count) {
                $step.reason = 'dependency_not_passed'; Save-ValidationReceipt; continue
            }
            $step.prerequisite_resolution = @(foreach ($tool in $step.prerequisites) {
                $cmd = Get-Command $tool -ErrorAction SilentlyContinue
                [ordered]@{ name = $tool; available = [bool]$cmd; path = if ($cmd) { $cmd.Source } else { $null } }
            })
            if (@($step.prerequisite_resolution | Where-Object { -not $_.available }).Count) {
                $step.reason = 'missing_tool'; Save-ValidationReceipt; continue
            }
            $step.started_at = [DateTime]::UtcNow.ToString('o'); $step.reason = 'interrupted'
            Save-ValidationReceipt
            Write-Host "== $($step.id)"
            try {
                & $script:Validation.Bodies[$step.id]
                $step.outcome = 'passed'; $step.reason = 'completed'
            } catch {
                # Do not serialize arbitrary exception messages (they may contain credentials).
                $reason = $_.Exception.Message
                if ($reason -in @('missing_tool','missing_database','missing_image')) {
                    $step.outcome = 'not_run'; $step.reason = $reason
                } else {
                    $step.outcome = 'failed'
                    $step.reason = if ($reason -in @('native_exit','malformed_output','semantic_failure','server_exited','readiness_timeout')) { $reason } else { 'powershell_exception' }
                }
            } finally {
                $step.ended_at = [DateTime]::UtcNow.ToString('o')
                $step.duration_ms = [long]([DateTime]::Parse($step.ended_at) - [DateTime]::Parse($step.started_at)).TotalMilliseconds
                Save-ValidationReceipt
            }
            Write-Host "$($step.id): $($step.outcome) ($($step.reason))"
        }
        $finished = $true
    } catch {
        $r.reason = 'runner_error'; $r.exit_code = 1
    } finally {
        $script:Validation.Current = $null
        try { Clear-ValidationResources } catch { $r.reason = 'cleanup_failed'; $r.exit_code = 1 }
        try {
            $r.source_after = Get-ValidationSource $script:Validation.Root $script:Validation.Inputs
            $r.source_changed = $null -eq $r.source_before -or $r.source_before.sha256 -ne $r.source_after.sha256 -or $r.source_before.commit -ne $r.source_after.commit
        } catch { $r.reason = 'source_identity_unavailable'; $r.exit_code = 1 }
        if ($r.exit_code -ne 1) {
            if (@($r.steps | Where-Object outcome -eq 'failed').Count) { $r.exit_code = 1; $r.reason = 'check_failed' }
            elseif (-not $finished -or $r.source_changed -or @($r.steps | Where-Object outcome -ne 'passed').Count) { $r.exit_code = 3; $r.reason = 'required_coverage_incomplete' }
            else { $r.exit_code = 0; $r.reason = 'selected_profile_passed' }
        }
        $r.ended_at = [DateTime]::UtcNow.ToString('o')
        $r.duration_ms = [long]([DateTime]::Parse($r.ended_at) - [DateTime]::Parse($r.started_at)).TotalMilliseconds
        # Terminal marker is written only after execution, cleanup and final source hash.
        $r.completed = $finished
        Save-ValidationReceipt
    }
    Write-Host "$($r.profile): $($r.reason); receipt: $($script:Validation.Path)"
    return [int]$r.exit_code
}

function Clear-ValidationResources {
    $failed = $false
    foreach ($process in $script:Validation.Processes) {
        try { if (-not $process.HasExited) { $process.Kill($true); $process.WaitForExit() } } catch { $failed = $true }
    }
    if ($script:Validation.Container) {
        try { Invoke-ValidationCommand docker @('rm','-f','-v',$script:Validation.Container) } catch { $failed = $true }
    }
    foreach ($resource in $script:Validation.Receipt.resources) { $resource.cleanup = if ($failed) { 'unverified' } else { 'completed' } }
    if ($failed) { throw 'cleanup_failed' }
}

function Assert-ValidationSummary {
    param([string]$Json)
    try { $value = ConvertFrom-Json -InputObject $Json -AsHashtable -ErrorAction Stop } catch { throw 'malformed_output' }
    if ($value -isnot [hashtable] -or -not $value.ContainsKey('passed') -or -not $value.ContainsKey('failed') -or
        ($value.passed -isnot [long] -and $value.passed -isnot [int]) -or
        ($value.failed -isnot [long] -and $value.failed -isnot [int]) -or
        $value.passed -lt 0 -or $value.failed -lt 0 -or ($value.passed + $value.failed) -eq 0) { throw 'malformed_output' }
    if ($value.failed -gt 0) { throw 'semantic_failure' }
}
