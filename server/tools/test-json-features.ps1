# SPDX-License-Identifier: Apache-2.0
# Two separate Cargo resolutions, real artifact exchange, optional isolated PG.
#Requires -Version 7
[CmdletBinding()]
param([switch]$Postgres, [string]$ReceiptPath)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/validation.ps1"
. "$PSScriptRoot/validation-tiers.ps1"
Push-Location (Split-Path $PSScriptRoot)
try {
    $root = Split-Path (Get-Location).Path
    $profile = if ($Postgres) { 'json-features+postgres' } else { 'json-features' }
    $notRequested = if ($Postgres) { @() } else { @('postgres') }
    New-ValidationRun $profile $root $ReceiptPath -NotRequested $notRequested -Inputs @(
        'server/tools/validation.ps1','server/tools/validation-tiers.ps1','server/tools/test-json-features.ps1',
        'server/src/munarium-server/src/json_persistence_tests.rs'
    )
    if ($Postgres) { Add-ValidationStep 'postgres.setup' { Start-ValidationPostgres } -Requires docker }
    foreach ($configuration in 'default','arbitrary') {
        $features = if ($configuration -eq 'arbitrary') { @('--features','json-arbitrary-precision') } else { @() }
        $body = {
            $graph = (Invoke-ValidationCommand cargo (@('tree','--locked','--offline','-p','munarium-server','-p','munarium-datastore','-e','features','-i','serde_json') + $features) -Capture) -join "`n"
            if (($graph -match 'serde_json feature "arbitrary_precision"') -ne ($configuration -eq 'arbitrary')) { throw 'feature_graph_mismatch' }
            [IO.File]::WriteAllText((Join-Path $script:Validation.Directory "$configuration-features.txt"), $graph)
        }.GetNewClosure()
        Add-ValidationStep "json.$configuration.graph" $body -Requires cargo
        $body = { Invoke-ValidationCargoTests (@('test','--locked','--offline','-p','munarium-server','-p','munarium-api-types','json_feature_') + $features) }.GetNewClosure()
        Add-ValidationStep "json.$configuration.dto" $body -Requires cargo -DependsOn "json.$configuration.graph"
        $authoringFeatures = if ($configuration -eq 'arbitrary') { @('--features','serde_json/arbitrary_precision') } else { @() }
        $body = {
            Invoke-ValidationCargoTests (@('test','--locked','--offline','-p','munarium-authoring') + $authoringFeatures)
        }.GetNewClosure()
        Add-ValidationStep "json.$configuration.authoring" $body -Requires cargo -DependsOn "json.$configuration.graph"
        $body = {
            Invoke-ValidationEnvironment @{ MUNARIUM_JSON_ARTIFACT_WRITE = (Join-Path $script:Validation.Directory "$configuration-artifact") } {
                Invoke-ValidationCargoTests (@('test','--locked','--offline','-p','munarium-datastore','--test','round_trip') + $features)
            }
        }.GetNewClosure()
        Add-ValidationStep "json.$configuration.artifact" $body -Requires cargo -DependsOn "json.$configuration.graph"
        if ($Postgres) {
            $body = {
                Invoke-ValidationEnvironment @{ MUNARIUM_TEST_DATABASE_URL = $script:Validation.DatabaseUrl } {
                    Invoke-ValidationCargoTests (@('test','--locked','--offline','-p','munarium-server','json_feature_pg_') + $features + @('--','--ignored'))
                }
            }.GetNewClosure()
            Add-ValidationStep "json.$configuration.postgres" $body -Requires cargo -DependsOn @('postgres.setup',"json.$configuration.graph")
        }
    }
    foreach ($configuration in 'default','arbitrary') {
        $features = if ($configuration -eq 'arbitrary') { @('--features','json-arbitrary-precision') } else { @() }
        $other = if ($configuration -eq 'arbitrary') { 'default' } else { 'arbitrary' }
        $body = {
            Invoke-ValidationEnvironment @{ MUNARIUM_JSON_ARTIFACT_READ = (Join-Path $script:Validation.Directory "$other-artifact") } {
                Invoke-ValidationCargoTests (@('test','--locked','--offline','-p','munarium-datastore','--test','round_trip','json_feature_artifact_read_other_configuration') + $features + @('--','--ignored'))
            }
        }.GetNewClosure()
        Add-ValidationStep "json.$configuration.read-$other" $body -Requires cargo -DependsOn @("json.$configuration.graph", "json.$other.artifact")
    }
    $code = Invoke-ValidationRun
} catch { Write-Error 'JSON qualification could not initialize.' -ErrorAction Continue; $code = 1 }
finally { Pop-Location }
exit $code
