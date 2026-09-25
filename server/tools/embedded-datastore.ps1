# SPDX-License-Identifier: Apache-2.0
# Validation steps for munarium-datastore's supported embedded tier (P15/D5).
#
# The consumer fixture at server/tests/embedded-datastore is its own Cargo
# workspace with its own committed lock, so these steps run cargo THERE. For
# each supported feature set they check the dependency closure, the serde_json
# feature graph, the tests on the pinned compiler with warnings denied, and the
# tests on the declared minimum compiler. They then exchange artifacts between
# the Server workspace's resolution and the fixture's. Mirrored step for step by
# the embedded-datastore job in .github/workflows/server-ci.yml: change both in
# the same commit. Policy: server/docs/embedded-support.md.

$script:EmbeddedFixture = 'tests/embedded-datastore'
$script:EmbeddedFeatureSets = [ordered]@{
    'no-default'               = @('--no-default-features')
    'default'                  = @()
    'vector-diskann'           = @('--features', 'vector-diskann')
    'json-arbitrary-precision' = @('--features', 'json-arbitrary-precision')
}

# The declared minimum compiler, read from the crate's own manifest so the
# checks cannot drift from the promise. Cargo reads `1.92` as 1.92.0, so the
# toolchain tested is exactly that release, not the latest 1.92.x.
function Get-EmbeddedDatastoreMsrv {
    $manifest = Get-Content -LiteralPath 'src/munarium-datastore/Cargo.toml' -Raw
    if ($manifest -notmatch '(?m)^rust-version\s*=\s*"([0-9]+\.[0-9]+)(\.[0-9]+)?"') { throw 'malformed_output' }
    if ($Matches[2]) { return $Matches[1] + $Matches[2] }
    return $Matches[1] + '.0'
}

function Invoke-EmbeddedFixture {
    param([scriptblock]$Body)
    Push-Location $script:EmbeddedFixture
    try { & $Body } finally { Pop-Location }
}

function Add-EmbeddedDatastoreSteps {
    $msrv = Get-EmbeddedDatastoreMsrv
    # Step bodies are closures created in this function, so they capture its
    # locals, not the runner's script scope: take the receipt directory here.
    $directory = $script:Validation.Directory
    Add-ValidationStep 'embedded.format' {
        Invoke-EmbeddedFixture { Invoke-ValidationCommand cargo @('fmt', '--check') }
    } -Requires cargo
    foreach ($set in $script:EmbeddedFeatureSets.Keys) {
        $flags = $script:EmbeddedFeatureSets[$set]
        $body = {
            Invoke-EmbeddedFixture {
                $lines = Invoke-ValidationCommand cargo (@('tree', '--locked', '-e', 'normal', '--prefix', 'none') + $flags) -Capture
                $names = @($lines | ForEach-Object { ($_ -split '\s+')[0] } | Sort-Object -Unique)
                if ($names.Count -eq 0) { throw 'malformed_output' }
                $allowed = @('munarium-datastore', 'munarium-datastore-consumer')
                $banned = @($names | Where-Object {
                        $_ -in @('sqlx', 'axum', 'tonic', 'reqwest', 'utoipa', 'openssl-sys') -or
                        ($_ -like 'munarium-*' -and $_ -notin $allowed)
                    })
                if ($banned.Count) { throw 'semantic_failure' }
            }
        }.GetNewClosure()
        Add-ValidationStep "embedded.$set.closure" $body -Requires cargo
        $body = {
            Invoke-EmbeddedFixture {
                $graph = (Invoke-ValidationCommand cargo (@('tree', '--locked', '-e', 'features', '-i', 'serde_json') + $flags) -Capture) -join "`n"
                # The fixture must not inherit Server's raw_value; arbitrary
                # precision appears only when that feature set asks for it.
                if ($graph -match 'serde_json feature "raw_value"') { throw 'semantic_failure' }
                if (($graph -match 'serde_json feature "arbitrary_precision"') -ne ($set -eq 'json-arbitrary-precision')) { throw 'semantic_failure' }
                [IO.File]::WriteAllText((Join-Path $directory "embedded-$set-serde-json.txt"), $graph)
            }
        }.GetNewClosure()
        Add-ValidationStep "embedded.$set.serde-json" $body -Requires cargo
        $body = {
            Invoke-EmbeddedFixture {
                # Path dependencies are not lint-capped, so this also denies
                # warnings in munarium-datastore under this feature set.
                Invoke-ValidationEnvironment @{ RUSTFLAGS = '-D warnings' } {
                    Invoke-ValidationCargoTests (@('test', '--locked') + $flags)
                }
            }
        }.GetNewClosure()
        Add-ValidationStep "embedded.$set.pinned" $body -Requires cargo
        $body = {
            $installed = Invoke-ValidationCommand rustup @('toolchain', 'list') -Capture
            if (-not @($installed | Where-Object { $_ -like "$msrv-*" }).Count) { throw 'missing_tool' }
            Invoke-EmbeddedFixture {
                Invoke-ValidationCargoTests (@("+$msrv", 'test', '--locked', '--target-dir', 'target/msrv') + $flags)
            }
        }.GetNewClosure()
        Add-ValidationStep "embedded.$set.msrv-$msrv" $body -Requires @('cargo', 'rustup')
    }
    # Artifacts written under one resolution must open under the other, in both
    # directions and both JSON configurations. Artifact ids are not compared:
    # the lexical index is not byte-reproducible (round_trip.rs
    # `two_builds_do_not_produce_identical_bytes`).
    foreach ($configuration in 'default', 'arbitrary') {
        $flags = if ($configuration -eq 'arbitrary') { @('--features', 'json-arbitrary-precision') } else { @() }
        $body = {
            Invoke-ValidationEnvironment @{ MUNARIUM_JSON_ARTIFACT_WRITE = (Join-Path $directory "workspace-$configuration") } {
                Invoke-ValidationCargoTests (@('test', '--locked', '-p', 'munarium-datastore', '--test', 'round_trip') + $flags + @('json_feature_artifact_write'))
            }
        }.GetNewClosure()
        Add-ValidationStep "embedded.exchange.$configuration.workspace-write" $body -Requires cargo
        $body = {
            Invoke-EmbeddedFixture {
                Invoke-ValidationEnvironment @{ MUNARIUM_JSON_ARTIFACT_WRITE = (Join-Path $directory "fixture-$configuration") } {
                    Invoke-ValidationCargoTests (@('test', '--locked', '--test', 'datastore_suite') + $flags + @('json_feature_artifact_write'))
                }
            }
        }.GetNewClosure()
        Add-ValidationStep "embedded.exchange.$configuration.fixture-write" $body -Requires cargo
        $body = {
            Invoke-ValidationEnvironment @{ MUNARIUM_JSON_ARTIFACT_READ = (Join-Path $directory "fixture-$configuration") } {
                Invoke-ValidationCargoTests (@('test', '--locked', '-p', 'munarium-datastore', '--test', 'round_trip') + $flags + @('json_feature_artifact_read_other_configuration', '--', '--ignored'))
            }
        }.GetNewClosure()
        Add-ValidationStep "embedded.exchange.$configuration.workspace-reads-fixture" $body -Requires cargo -DependsOn "embedded.exchange.$configuration.fixture-write"
        $body = {
            Invoke-EmbeddedFixture {
                Invoke-ValidationEnvironment @{ MUNARIUM_JSON_ARTIFACT_READ = (Join-Path $directory "workspace-$configuration") } {
                    Invoke-ValidationCargoTests (@('test', '--locked', '--test', 'datastore_suite') + $flags + @('json_feature_artifact_read_other_configuration', '--', '--ignored'))
                }
            }
        }.GetNewClosure()
        Add-ValidationStep "embedded.exchange.$configuration.fixture-reads-workspace" $body -Requires cargo -DependsOn "embedded.exchange.$configuration.workspace-write"
    }
}
