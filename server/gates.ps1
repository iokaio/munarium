# SPDX-License-Identifier: Apache-2.0
# Local gate profile. CI retains its independent automatic inventory.
#Requires -Version 7
[CmdletBinding()]
param([string]$ReceiptPath)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/tools/validation.ps1"
. "$PSScriptRoot/tools/validation-tiers.ps1"
. "$PSScriptRoot/tools/embedded-datastore.ps1"
. "$PSScriptRoot/tools/gate-catalog.ps1"
Push-Location $PSScriptRoot
try {
    New-ValidationRun 'gates' (Split-Path $PSScriptRoot) $ReceiptPath -Inputs @('server/tools/validation.ps1','server/tools/validation-tiers.ps1','server/tools/embedded-datastore.ps1')
    Add-CatalogSteps @('runner.regression','catalog.regression','catalog.equivalence','format','clippy.default','clippy.all-features','clippy.diskann')
    Add-ValidationTiers $true $true $true $true $true
    Add-ValidationStep 'docs.openapi' {
        $json = (Invoke-ValidationCommand cargo @('run','-q','-p','munarium-server','--','openapi') -Capture) -join "`n"
        $tmp = Join-Path $script:Validation.Directory 'openapi.json'
        [IO.File]::WriteAllText($tmp, $json)
        Invoke-ValidationCommand py @('-c',"import json,sys; a=json.load(open(sys.argv[1],encoding='utf-8-sig')); b=json.load(open(sys.argv[2],encoding='utf-8-sig')); sys.exit(0 if a==b else 'OpenAPI is stale')",$tmp,'docs/api/openapi.json')
    } -Requires @('cargo','py')
    Add-CatalogSteps @('contract.mmp','contract.matrix','license')
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
    Add-CatalogSteps @('boundaries.crates','boundaries.datastore','boundaries.retrieval','migrations.additive')
    # The supported embedded datastore tier, from its isolated consumer. A
    # missing minimum toolchain is incomplete required coverage (3); CI's
    # embedded-datastore job mirrors these steps.
    Add-EmbeddedDatastoreSteps
    # Missing local cargo-deny is incomplete required coverage (3); CI still enforces it.
    Add-CatalogSteps @('dependencies.deny')
    $code = Invoke-ValidationRun
} catch { Write-Error 'Validation runner could not initialize; no successful receipt.' -ErrorAction Continue; $code = 1 }
finally { Pop-Location }
exit $code
