# SPDX-License-Identifier: Apache-2.0
# Uses the shared execution contract; no work runs when dot-sourced.
function Assert-MatrixEnvironment {
    param([string[]]$Names)
    foreach ($name in $Names) {
        if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($name))) { throw 'missing_environment' }
    }
}

function Invoke-MatrixTests {
    param([string[]]$Arguments)
    $output = (Invoke-ValidationCommand cargo $Arguments -Capture) -join "`n"
    $log = Join-Path $script:Validation.Directory $script:Validation.Current.commands[-1].raw_local_log
    if ((Get-Content -LiteralPath $log -Raw) -match '(?im)^\s*(SKIPPED:|unavailable:)') { throw 'missing_environment' }
    $summaries = [regex]::Matches($output, '(?m)^test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;')
    if ($summaries.Count -eq 0) { throw 'malformed_output' }
    $passed = 0L; $failed = 0L; $ignored = 0L
    foreach ($summary in $summaries) {
        $passed += [long]$summary.Groups[1].Value
        $failed += [long]$summary.Groups[2].Value
        $ignored += [long]$summary.Groups[3].Value
    }
    if ($passed -eq 0 -or $failed -ne 0) { throw 'semantic_failure' }
    $script:Validation.Current['test_summary'] = @{ passed=$passed; failed=$failed; ignored=$ignored }
    Write-Host "Matrix: $passed passed; $ignored ignored (not executed)."
}

function Start-MatrixValidationPostgres {
    $image = 'pgvector/pgvector:pg16@sha256:ccc6e83d6e35e931dc7c5def2022729d5a6c370318d099181995567ff1fb4d6b'
    Invoke-ValidationCommand docker @('info','--format','{{.ServerVersion}}')
    $images = @(Invoke-ValidationCommand docker @('image','ls','--digests','--format','{{.Repository}}@{{.Digest}}','--filter','reference=pgvector/pgvector') -Capture)
    if (($image -replace ':pg16@','@') -notin $images) { throw 'missing_image' }
    # Copy only tracked public fixture SQL; never mount a local untracked addition.
    $fixture = Join-Path $script:Validation.Directory 'matrix-sql'
    [void][IO.Directory]::CreateDirectory($fixture)
    $paths = @(Invoke-ValidationCommand git @('-C',$script:Validation.Root,'ls-files','matrix/fixtures/t0/sql/*.sql') -Capture)
    if ($paths.Count -eq 0) { throw 'malformed_output' }
    foreach ($path in $paths) {
        if ($path -notmatch '^matrix/fixtures/t0/sql/[^/]+\.sql$') { throw 'malformed_output' }
        Copy-Item -LiteralPath (Join-Path $script:Validation.Root $path) -Destination $fixture
    }
    $name = 'munarium-matrix-validation-' + $script:Validation.Receipt.run_id
    $id = (Invoke-ValidationCommand docker @('create','--name',$name,'--label',"munarium.validation=$name",'--publish','127.0.0.1::5432','--mount',"type=bind,source=$fixture,target=/docker-entrypoint-initdb.d,readonly",'-e','POSTGRES_USER=matrix','-e','POSTGRES_PASSWORD=matrix-dev','-e','POSTGRES_DB=matrix',$image,'postgres','-c','wal_level=logical') -Capture) -join ''
    if ($id -notmatch '^[a-f0-9]{64}$') { throw 'malformed_output' }
    $script:Validation.Container = $id
    $script:Validation.Receipt.resources += [ordered]@{kind='container'; id=$id; name=$name; cleanup='pending'}
    Save-ValidationReceipt
    Invoke-ValidationCommand docker @('start',$id)
    $binding = (Invoke-ValidationCommand docker @('port',$id,'5432/tcp') -Capture) -join ''
    if ($binding -notmatch '^127\.0\.0\.1:(\d+)$') { throw 'malformed_output' }
    $script:Validation.DatabaseUrl = "postgres://matrix_owner:matrix-owner-dev@127.0.0.1:$($Matches[1])/matrix"
    foreach ($i in 1..90) {
        $rows = (Invoke-ValidationCommand docker @('exec','-e','PGPASSWORD=matrix-dev',$id,'psql','-h','127.0.0.1','-U','matrix','-d','matrix','-tAc',"SELECT count(*) FROM pg_publication WHERE pubname LIKE 'munarium_matrix_%'") -Capture -AcceptExitCodes @(0,1,2)) -join ''
        if ($script:Validation.Current.commands[-1].exit_code -eq 0 -and "$rows".Trim() -match '^[1-9][0-9]*$') { return }
        Start-Sleep -Seconds 1
    }
    throw 'readiness_timeout'
}

function Invoke-MatrixPostgresTests {
    param([string]$Filter)
    Invoke-ValidationEnvironment @{MUNARIUM_MATRIX_TEST_DATABASE_URL=$script:Validation.DatabaseUrl; MUNARIUM_MATRIX_TEST_HTTP=$null} {
        Invoke-MatrixTests @('test','-p','munarium-matrix-conformance',$Filter,'--','--ignored','--nocapture')
    }
}

function Add-MatrixValidationTiers {
    param([bool]$Postgres, [bool]$BlackBox, [bool]$Gates, [bool]$Browser, [bool]$MySql)
    $script:MatrixPython = 'munarium-unavailable-python'
    foreach ($candidate in 'py','python3','python') {
        if (-not (Get-Command $candidate -ErrorAction SilentlyContinue)) { continue }
        try { & $candidate --version *> $null; if ($LASTEXITCODE -eq 0) { $script:MatrixPython=$candidate; break } } catch { }
    }
    if ($Gates) {
        Add-ValidationStep 'matrix.fmt' { Invoke-ValidationCommand cargo @('fmt','--all','--','--check') } -Requires cargo
        Add-ValidationStep 'matrix.clippy' { Invoke-ValidationCommand cargo @('clippy','--workspace','--all-targets','--','-D','warnings') } -Requires cargo
    }
    Add-ValidationStep 'matrix.workspace' {
        $values = @{}
        foreach ($entry in Get-ChildItem Env:MUNARIUM_MATRIX_TEST_*) { $values[$entry.Name]=$null }
        Invoke-ValidationEnvironment $values { Invoke-MatrixTests @('test','--workspace','--','--nocapture') }
    } -Requires cargo
    foreach ($check in @(
        @('boundaries','scripts/boundaries.py'),
        @('examples','contract/validate_examples.py'),
        @('publisher-self-test','contract/publish.py','--self-test'),
        @('publisher-drift','contract/publish.py','--check','../server/contract/matrix'),
        @('license','check_license.py'),
        @('notices','scripts/third_party_notices.py','--check','--cargo-target','x86_64-unknown-linux-musl'),
        @('doclint','scripts/doclint.py')
    )) {
        $argsForCheck = @($check | Select-Object -Skip 1)
        $pythonForCheck = $script:MatrixPython
        Add-ValidationStep "matrix.$($check[0])" { Invoke-ValidationCommand $pythonForCheck $argsForCheck }.GetNewClosure() -Requires $script:MatrixPython
    }
    Add-ValidationStep 'matrix.runner-controls' {
        Invoke-ValidationCommand $script:MatrixPython @('-m','unittest','discover','-s','tools','-p','test_validation.py')
    } -Requires @($script:MatrixPython, 'pwsh')
    Add-ValidationStep 'matrix.openapi' { Invoke-ValidationCommand cargo @('run','-q','-p','munarium-matrix-server','--bin','munarium-matrix','--','openapi','--check','docs/api/openapi.json') } -Requires cargo
    if ($Postgres -or $BlackBox) {
        if ($BlackBox) {
            Add-ValidationStep 'matrix.postgres.setup' {
                Assert-MatrixEnvironment @('MUNARIUM_MATRIX_TEST_DATABASE_URL')
                $script:Validation.DatabaseUrl = $env:MUNARIUM_MATRIX_TEST_DATABASE_URL
            }
        } else {
            Add-ValidationStep 'matrix.postgres.setup' { Start-MatrixValidationPostgres } -Requires docker
        }
        foreach ($scope in 'postgres','cdc') {
            $filter = "scenarios::${scope}::"
            Add-ValidationStep "matrix.$scope" { Invoke-MatrixPostgresTests $filter }.GetNewClosure() -Requires cargo -DependsOn 'matrix.postgres.setup'
        }
    }
    if ($BlackBox) {
        Add-ValidationStep 'matrix.blackbox.environment' {
            Assert-MatrixEnvironment @('MUNARIUM_MATRIX_TEST_DATABASE_URL','MUNARIUM_MATRIX_TEST_URL','MUNARIUM_MATRIX_TEST_TOKEN','MUNARIUM_MATRIX_TEST_MGMT_TOKEN','MUNARIUM_MATRIX_TEST_GRPC','MUNARIUM_MATRIX_TEST_SQLSERVER')
        } -DependsOn 'matrix.postgres.setup'
        Add-ValidationStep 'matrix.blackbox' {
            Invoke-ValidationEnvironment @{MUNARIUM_MATRIX_TEST_HTTP='1'} {
                Invoke-MatrixTests @('test','-p','munarium-matrix-conformance','--','--include-ignored','--skip','scenarios::mysql::','--skip','measure::','--nocapture')
            }
        } -Requires cargo -DependsOn 'matrix.blackbox.environment'
    }
    if ($MySql) {
        Add-ValidationStep 'matrix.mysql' {
            Assert-MatrixEnvironment @('MUNARIUM_MATRIX_TEST_MYSQL')
            Invoke-MatrixTests @('test','-p','munarium-matrix-conformance','scenarios::mysql::','--','--ignored','--nocapture')
        } -Requires cargo
    }
    if ($Browser) {
        Add-ValidationStep 'matrix.browser' {
            Assert-MatrixEnvironment @('MUNARIUM_MATRIX_TEST_URL','MUNARIUM_MATRIX_TEST_TOKEN','MUNARIUM_MATRIX_TEST_MGMT_TOKEN')
            Invoke-ValidationEnvironment @{MUNARIUM_MATRIX_SHOTS=(Join-Path $script:Validation.Directory 'browser-shots')} {
                Invoke-ValidationCommand node @('ui-smoke/smoke.mjs')
            }
        } -Requires node
    }
}
