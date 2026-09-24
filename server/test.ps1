# SPDX-License-Identifier: Apache-2.0
# Offline by default. Exit 0: selected profile passed; 1: failed; 3: incomplete.
#Requires -Version 7
[CmdletBinding()]
param([switch]$Postgres, [switch]$BlackBox, [Alias('Enterprise')][switch]$Platform, [switch]$Cluster, [switch]$All, [string]$ReceiptPath)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/tools/validation.ps1"
. "$PSScriptRoot/tools/validation-tiers.ps1"
if ($All) { $Postgres = $BlackBox = $Platform = $Cluster = $true }
$selected = @('offline'); $notRequested = @()
foreach ($tier in 'Postgres','BlackBox','Platform','Cluster') {
    if ((Get-Variable $tier).Value) { $selected += $tier.ToLowerInvariant() } else { $notRequested += $tier.ToLowerInvariant() }
}
Push-Location $PSScriptRoot
try {
    New-ValidationRun ($selected -join '+') (Split-Path $PSScriptRoot) $ReceiptPath -NotRequested $notRequested -Inputs @('server/tools/validation.ps1','server/tools/validation-tiers.ps1')
    Add-ValidationTiers $Postgres $BlackBox $Platform $Cluster
    $code = Invoke-ValidationRun
} catch { Write-Error 'Validation runner could not initialize; no successful receipt.' -ErrorAction Continue; $code = 1 }
finally { Pop-Location }
exit $code
