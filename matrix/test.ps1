# SPDX-License-Identifier: Apache-2.0
# Profile-scoped receipts: 0 passed, 1 failed, 3 required coverage unavailable.
#Requires -Version 7
[CmdletBinding()]
param(
    [switch]$Postgres, [switch]$BlackBox, [switch]$Gates,
    [switch]$Browser, [switch]$MySql, [switch]$All, [string]$ReceiptPath
)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../server/tools/validation.ps1"
. "$PSScriptRoot/tools/validation-tiers.ps1"
if ($All) { $Postgres = $BlackBox = $Gates = $MySql = $true }
$selected = @('matrix.offline'); $notRequested = @('measurement')
foreach ($tier in 'Postgres','BlackBox','Gates','Browser','MySql') {
    if ((Get-Variable $tier).Value) { $selected += $tier.ToLowerInvariant() }
    else { $notRequested += $tier.ToLowerInvariant() }
}
Push-Location $PSScriptRoot
try {
    New-ValidationRun ($selected -join '+') (Split-Path $PSScriptRoot) $ReceiptPath -NotRequested $notRequested -Inputs @('matrix/tools/validation-tiers.ps1','matrix/tools/test_validation.py')
    Add-MatrixValidationTiers $Postgres $BlackBox $Gates $Browser $MySql
    $code = Invoke-ValidationRun
} catch {
    Write-Error 'Matrix validation could not initialize; no successful receipt.' -ErrorAction Continue
    $code = 1
} finally { Pop-Location }
exit $code
