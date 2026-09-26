# SPDX-License-Identifier: Apache-2.0
# Register portable commands directly so receipts retain argv and native status.
$script:GateCatalogPath = Join-Path $PSScriptRoot 'gate-catalog.json'
function Invoke-CatalogCommand {
    param([string]$Id, [switch]$Tests)
    $catalog = Get-Content -LiteralPath $script:GateCatalogPath -Raw | ConvertFrom-Json
    $matches = @($catalog.steps | Where-Object id -EQ $Id)
    if ($matches.Count -ne 1) { throw 'invalid_catalog' }
    Push-Location (Join-Path $script:Validation.Root $matches[0].cwd)
    try {
        foreach ($command in $matches[0].commands) {
            $exe = if ($command[0] -eq '{python}') { 'py' } else { $command[0] }
            $arguments = @($command | Select-Object -Skip 1)
            if ($Tests) {
                if ($exe -ne 'cargo') { throw 'invalid_catalog' }
                Invoke-ValidationCargoTests $arguments
            } else { Invoke-ValidationCommand $exe $arguments }
        }
    } finally { Pop-Location }
}
function Add-CatalogSteps {
    param([string[]]$Ids)
    $catalog = Get-Content -LiteralPath $script:GateCatalogPath -Raw | ConvertFrom-Json
    if ($catalog.schema_version -ne 1) { throw 'invalid_catalog' }
    foreach ($id in $Ids) {
        $matches = @($catalog.steps | Where-Object id -EQ $id)
        if ($matches.Count -ne 1 -or -not $matches[0].required) { throw 'invalid_catalog' }
        $entry = $matches[0]
        $root = $script:Validation.Root
        $body = {
            Push-Location (Join-Path $root $entry.cwd)
            try {
                foreach ($command in $entry.commands) {
                    $exe = if ($command[0] -eq '{python}') { 'py' } else { $command[0] }
                    Invoke-ValidationCommand $exe @($command | Select-Object -Skip 1)
                }
            } finally { Pop-Location }
        }.GetNewClosure()
        Add-ValidationStep $id $body -Requires $entry.prerequisites -DependsOn $entry.depends_on
    }
}
