# SPDX-License-Identifier: Apache-2.0
# Local gate profile. CI retains its independent automatic inventory.
#Requires -Version 7
[CmdletBinding()]
param([string]$ReceiptPath)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/tools/validation.ps1"
. "$PSScriptRoot/tools/validation-tiers.ps1"
Push-Location $PSScriptRoot
try {
    New-ValidationRun 'gates' (Split-Path $PSScriptRoot) $ReceiptPath -Inputs @('server/tools/validation.ps1','server/tools/validation-tiers.ps1')
    Add-ValidationStep 'runner.regression' { Invoke-ValidationCommand py @('-m','unittest','discover','-s','tools','-p','test_validation.py') } -Requires @('py','pwsh')
    Add-ValidationStep 'format' { Invoke-ValidationCommand cargo @('fmt','--all','--check') } -Requires cargo
    Add-ValidationStep 'clippy.default' { Invoke-ValidationCommand cargo @('clippy','--workspace','--all-targets','--','-D','warnings') } -Requires cargo
    Add-ValidationStep 'clippy.all-features' { Invoke-ValidationCommand cargo @('clippy','--workspace','--all-features','--all-targets','--','-D','warnings') } -Requires cargo
    Add-ValidationTiers $true $true $true $true $true
    Add-ValidationStep 'docs.openapi' {
        $json = (Invoke-ValidationCommand cargo @('run','-q','-p','munarium-server','--','openapi') -Capture) -join "`n"
        $tmp = Join-Path $script:Validation.Directory 'openapi.json'
        [IO.File]::WriteAllText($tmp, $json)
        Invoke-ValidationCommand py @('-c',"import json,sys; a=json.load(open(sys.argv[1],encoding='utf-8-sig')); b=json.load(open(sys.argv[2],encoding='utf-8-sig')); sys.exit(0 if a==b else 'OpenAPI is stale')",$tmp,'docs/api/openapi.json')
    } -Requires @('cargo','py')
    Add-ValidationStep 'contract.mmp' { Invoke-ValidationCommand py @('contract/mmp/publish.py','--self-test') } -Requires py
    Add-ValidationStep 'contract.matrix' { Invoke-ValidationCommand py @('../matrix/contract/publish.py','--check','contract/matrix') } -Requires py
    Add-ValidationStep 'license' { Invoke-ValidationCommand py @('check_license.py') } -Requires py
    Add-ValidationStep 'notices' {
        # The generator recursively discovers workspaces. Give it exactly the
        # declared Server source, excluding ignored scratch checkouts.
        $snapshot = Join-Path $script:Validation.Directory 'notices-source'
        try {
            foreach ($input in $script:Validation.Receipt.source_before.inputs) {
                if ($input.path -notlike 'server/*' -or $input.sha256 -eq 'missing') { continue }
                $destination = Join-Path $snapshot $input.path
                [void][IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination))
                [IO.File]::Copy((Join-Path $script:Validation.Root $input.path), $destination)
            }
            Invoke-ValidationCommand py @((Join-Path $snapshot 'server/tools/third_party_notices.py'),'--check','--cargo-target','x86_64-unknown-linux-musl')
        } finally {
            $absolute = [IO.Path]::GetFullPath($snapshot)
            $owner = [IO.Path]::GetFullPath($script:Validation.Directory).TrimEnd('/','\') + [IO.Path]::DirectorySeparatorChar
            if (-not $absolute.StartsWith($owner)) { throw 'snapshot_cleanup_outside_run' }
            if (Test-Path -LiteralPath $absolute) { Remove-Item -LiteralPath $absolute -Recurse -Force }
        }
    } -Requires @('py','cargo')
    Add-ValidationStep 'docs.grpc' {
        $tmp = Join-Path $script:Validation.Directory 'grpc-reference.md'
        Invoke-ValidationCommand cargo @('run','-q','-p','munarium-proto','--bin','gen-grpc-docs','--',$tmp)
        Invoke-ValidationCommand git @('diff','--no-index','--exit-code','--','docs/api/grpc-reference.md',$tmp)
    } -Requires @('cargo','git')
    Add-ValidationStep 'boundaries.crates' {
        foreach ($crate in 'munarium-core','munarium-access','munarium-providers','munarium-api-types','munarium-datastore') {
            $arguments = @('tree','-p',$crate,'-e','normal','--prefix','none')
            if ($crate -eq 'munarium-api-types') { $arguments += '--all-features' }
            $lines = Invoke-ValidationCommand cargo $arguments -Capture
            $deps = @($lines | ForEach-Object { ($_ -split '\s+')[0] } | Sort-Object -Unique)
            if ($deps.Count -eq 0) { throw 'malformed_output' }
            $banned = switch ($crate) {
                munarium-providers { @('munarium-store-pg','munarium-store-mem','munarium-retrieval-pg') }
                munarium-api-types { @($deps | Where-Object { $_ -like 'munarium-*' -and $_ -notin @('munarium-api-types','munarium-proto') }) }
                munarium-datastore { @('sqlx','axum','tonic','reqwest','utoipa','munarium-core','munarium-store-pg','munarium-retrieval-pg','munarium-server','munarium-runbooks') }
                default { @('sqlx','axum','tonic','reqwest','utoipa') }
            }
            if (@($deps | Where-Object { $_ -in $banned }).Count) { throw 'crate_boundary_violation' }
        }
    } -Requires cargo
    Add-ValidationStep 'boundaries.retrieval' {
        $allowed = [IO.Path]::GetFullPath((Join-Path $PWD 'src/munarium-server/src/state.rs'))
        $strays = Get-ChildItem -LiteralPath 'src/munarium-server/src' -Filter '*.rs' -Recurse -File |
            Select-String -Pattern 'munarium_retrieval_pg' -List | Where-Object { $_.Path -ne $allowed }
        if ($strays) { throw 'retrieval_boundary_violation' }
    }
    Add-ValidationStep 'migrations.additive' {
        $bad = Get-ChildItem -LiteralPath 'src/munarium-store-pg/migrations' -File -Recurse |
            Select-String -Pattern '^\s*(drop\s+(table|column)|alter\s+table\s+\S+\s+drop)'
        if ($bad) { throw 'destructive_ddl' }
    }
    # Missing local cargo-deny is incomplete required coverage (3); CI still enforces it.
    Add-ValidationStep 'dependencies.deny' { Invoke-ValidationCommand cargo @('deny','check') } -Requires @('cargo','cargo-deny')
    $code = Invoke-ValidationRun
} catch { Write-Error 'Validation runner could not initialize; no successful receipt.' -ErrorAction Continue; $code = 1 }
finally { Pop-Location }
exit $code
