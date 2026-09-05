# SPDX-License-Identifier: Apache-2.0
# gates.ps1 — the munarium-server local gate set, the same gates server-ci's
# lint-test and cargo-deny jobs run, against the same pg container. This is the
# product's full local gate runner; it deploys nothing.
#
#   ./gates.ps1                     # every gate, deploy nothing; exit non-zero on the first failure
#
# Prereqs: Docker Desktop running (the compose postgres on :5433), Rust stable,
# the py launcher. cargo-deny is optional (warns if absent; CI still enforces it).
#
# The pg gates execute against a DISPOSABLE database recreated every run: CI
# gets a clean schema for free from a fresh container, but a dev box keeps its
# pgdata volume for weeks, and a long-lived database accumulates whatever
# migration history it happened to see. Reusing it would make the gate depend
# on this machine's past rather than on the code under test.
#
# Every step here is mirrored in .github/workflows/server-ci.yml: change them in the same
# commit, or a gate that passes locally and fails in CI -- or the reverse --
# is what you get.
#Requires -Version 7
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$Server = $PSScriptRoot
$TestDb    = 'munarium_ci'
$TestDbUrl = "postgres://munarium:munarium-dev@localhost:5433/$TestDb"

function Step {
    param([string]$Name, [scriptblock]$Body)
    Write-Host "`n==> $Name" -ForegroundColor Cyan
    $global:LASTEXITCODE = 0
    & $Body
    if ($LASTEXITCODE -ne 0) { throw "FAILED: $Name (exit $LASTEXITCODE)" }
}

Push-Location $Server
try {
# ---- the lint-test job, verbatim gates ---------------------------------
Step 'postgres test container (docker compose, :5433)' {
    docker compose up -d postgres
    if ($LASTEXITCODE -ne 0) { return }
    $ok = $false
    foreach ($i in 1..30) {
        docker compose exec -T postgres pg_isready -U munarium *> $null
        if ($LASTEXITCODE -eq 0) { $ok = $true; break }
        Start-Sleep 1
    }
    if (-not $ok) { throw 'postgres never became ready' }

    # Recreate the test database so migrations always apply from zero.
    # Without this, editing a shipped migration (legitimate pre-1.0) makes
    # sqlx refuse the whole run with "migration N ... has been modified",
    # and the failure looks like a code defect rather than stale local state.
    docker compose exec -T postgres psql -U munarium -d postgres `
        -c "DROP DATABASE IF EXISTS $TestDb WITH (FORCE)" `
        -c "CREATE DATABASE $TestDb OWNER munarium" *> $null
    if ($LASTEXITCODE -ne 0) { throw "could not recreate the $TestDb test database" }
    Write-Host "  test database $TestDb recreated (clean migration run)" -ForegroundColor DarkGray
    $global:LASTEXITCODE = 0
}

Step 'fmt' { cargo fmt --all --check }
Step 'clippy' { cargo clippy --workspace --all-targets -- -D warnings }
# Every optional feature, compiled -- the default graph does not build `ocr`
# or `vector-diskann`, so nothing else here would notice a defect in them.
# Mirrored in server-ci.yml; change both in the same commit.
Step 'clippy (all features)' { cargo clippy --workspace --all-features --all-targets -- -D warnings }

Step 'test (includes pg integration)' {
    $env:MUNARIUM_TEST_DATABASE_URL = $TestDbUrl
    cargo test --workspace
}

Step 'conformance (in-process + postgres)' {
    cargo run -p mmp-conformance -- --in-process
    if ($LASTEXITCODE -ne 0) { return }
    cargo run -p mmp-conformance -- --postgres $TestDbUrl
}

Step 'conformance (black-box, both planes, pg-backed)' {
    # Pre-build both binaries (same race CI dodges), run the server on the
    # 18080/15051 alternates (8080 is taken on this box).
    cargo build -p munarium-server -p mmp-conformance
    if ($LASTEXITCODE -ne 0) { return }
    # 18080/15051/19090 are this script's own alternate test ports, so an
    # munarium-server listening there is by definition a stale test instance
    # (this gate, the clients-conformance recipe, or an interrupted run) —
    # reap it instead of making a human do it. Anything ELSE on the port
    # is not ours to kill and still stops the run.
    foreach ($port in 18080, 15051, 19090) {
        $conn = Get-NetTCPConnection -State Listen -LocalPort $port -ErrorAction SilentlyContinue
        if ($conn) {
            $ownerPid = $conn[0].OwningProcess
            $owner = (Get-Process -Id $ownerPid -ErrorAction SilentlyContinue).ProcessName
            if ($owner -eq 'munarium-server') {
                Write-Host "  reaping leftover munarium-server (pid $ownerPid) on port $port" -ForegroundColor DarkGray
                Stop-Process -Id $ownerPid -Force -ErrorAction SilentlyContinue
                Start-Sleep -Milliseconds 500
            }
            else {
                throw "port $port is already in use by pid $ownerPid ($owner) — not an munarium-server, so not stopping it for you"
            }
        }
    }
    $outLog = Join-Path ([IO.Path]::GetTempPath()) 'munarium-blackbox-server.out.log'
    $errLog = Join-Path ([IO.Path]::GetTempPath()) 'munarium-blackbox-server.err.log'
    # Bytes in Postgres for the local black-box run: under MUNARIUM_STORE=
    # postgres the source store DEFAULTS to 'az' and fails closed without
    # an account — correct for deployments, wrong for a laptop. Say pg
    # explicitly, exactly like the compose file does.
    $env:MUNARIUM_SOURCE_STORE = 'pg'
    $env:MUNARIUM_HTTP_ADDR = '127.0.0.1:18080'
    $env:MUNARIUM_GRPC_ADDR = '127.0.0.1:15051'
    $env:MUNARIUM_OPS_ADDR = '127.0.0.1:19090'
    $env:MUNARIUM_STORE = 'postgres'
    $env:MUNARIUM_DATABASE_URL = $TestDbUrl
    $env:MUNARIUM_AUTH_MODE = 'static'
    $env:MUNARIUM_STATIC_TOKENS = 'citoken:ci-local:rw'
    $proc = Start-Process -FilePath (Join-Path $Server 'target\debug\munarium-server.exe') `
        -WorkingDirectory $Server -PassThru -NoNewWindow `
        -RedirectStandardOutput $outLog -RedirectStandardError $errLog
    try {
        # Wait for BOTH planes (/healthz answers before tonic binds
        # :15051). NOTE the gRPC endpoint below MUST carry the http://
        # scheme: tonic's connector tolerates a scheme-less endpoint on
        # Linux (CI) but fails it on Windows with "transport error"
        # (verified live 2026-08-09).
        $ready = $false
        foreach ($i in 1..30) {
            if ($proc.HasExited) { break }
            $restUp = $false
            try { Invoke-RestMethod 'http://127.0.0.1:18080/healthz' | Out-Null; $restUp = $true } catch {}
            if ($restUp) {
                $tcp = [System.Net.Sockets.TcpClient]::new()
                try { $tcp.Connect('127.0.0.1', 15051); $ready = $true } catch {}
                finally { $tcp.Dispose() }
                if ($ready) { break }
            }
            Start-Sleep 1
        }
        if (-not $ready) {
            Write-Host '--- server stderr (tail) ---' -ForegroundColor Yellow
            Get-Content $errLog -Tail 40 -ErrorAction SilentlyContinue | Write-Host
            if ($proc.HasExited) { throw "server exited early (code $($proc.ExitCode)) — full logs: $outLog / $errLog" }
            throw "server never became ready on both planes — full logs: $outLog / $errLog"
        }
        & (Join-Path $Server 'target\debug\mmp-conformance.exe') `
            --http http://127.0.0.1:18080 --grpc http://127.0.0.1:15051 --token citoken
        if ($LASTEXITCODE -ne 0) {
            Write-Host '--- server stderr (tail) ---' -ForegroundColor Yellow
            Get-Content $errLog -Tail 40 -ErrorAction SilentlyContinue | Write-Host
        }
    }
    finally {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        'MUNARIUM_HTTP_ADDR','MUNARIUM_GRPC_ADDR','MUNARIUM_OPS_ADDR','MUNARIUM_STORE',
        'MUNARIUM_SOURCE_STORE','MUNARIUM_DATABASE_URL','MUNARIUM_AUTH_MODE','MUNARIUM_STATIC_TOKENS' |
            ForEach-Object { Remove-Item "env:$_" -ErrorAction SilentlyContinue }
    }
}

Step 'conformance (platform surface, pg-backed)' {
    # Mirrors server-ci.yml 'conformance (platform surface)' and
    # test.ps1 -Platform — change all three in the same commit. The
    # 2026-08-11 lesson: this suite once existed only in test.ps1, so a
    # run-breaking resolveSources bug shipped to main with CI green. The
    # binaries were built by the black-box step above; 18081/19091 are the
    # platform-tier alternates (same reaping rule as the black-box step).
    foreach ($port in 18081, 19091) {
        $conn = Get-NetTCPConnection -State Listen -LocalPort $port -ErrorAction SilentlyContinue
        if ($conn) {
            $ownerPid = $conn[0].OwningProcess
            $owner = (Get-Process -Id $ownerPid -ErrorAction SilentlyContinue).ProcessName
            if ($owner -eq 'munarium-server') {
                Write-Host "  reaping leftover munarium-server (pid $ownerPid) on port $port" -ForegroundColor DarkGray
                Stop-Process -Id $ownerPid -Force -ErrorAction SilentlyContinue
                Start-Sleep -Milliseconds 500
            }
            else {
                throw "port $port is already in use by pid $ownerPid ($owner) — not an munarium-server, so not stopping it for you"
            }
        }
    }
    $entLog = Join-Path ([IO.Path]::GetTempPath()) 'munarium-ent-gate-server.err.log'
    # Fresh tenant per run so scenarios never collide with earlier data
    # (same rule as test.ps1 and CI's ci-$GITHUB_RUN_ID tenants).
    $entTenant = "ent-gate-$(Get-Random)"
    $env:MUNARIUM_HTTP_ADDR = '127.0.0.1:18081'
    $env:MUNARIUM_GRPC_ADDR = 'disabled'
    $env:MUNARIUM_OPS_ADDR = '127.0.0.1:19091'
    $env:MUNARIUM_STORE = 'postgres'
    $env:MUNARIUM_SOURCE_STORE = 'pg'
    $env:MUNARIUM_DATABASE_URL = $TestDbUrl
    $env:MUNARIUM_AUTH_MODE = 'static'
    $env:MUNARIUM_STATIC_TOKENS = "ent-rw:${entTenant}:rw,ent-mgmt:${entTenant}:mgmt"
    $env:MUNARIUM_TOKEN_SECRET = 'ci-platform-token-secret-32-bytes!!'
    $proc = Start-Process -FilePath (Join-Path $Server 'target\debug\munarium-server.exe') `
        -WorkingDirectory $Server -PassThru -NoNewWindow `
        -RedirectStandardError $entLog
    try {
        $ready = $false
        foreach ($i in 1..30) {
            if ($proc.HasExited) { break }
            try { Invoke-RestMethod 'http://127.0.0.1:18081/healthz' | Out-Null; $ready = $true; break } catch {}
            Start-Sleep 1
        }
        if (-not $ready) {
            Write-Host '--- platform server stderr (tail) ---' -ForegroundColor Yellow
            Get-Content $entLog -Tail 40 -ErrorAction SilentlyContinue | Write-Host
            if ($proc.HasExited) { throw "platform server exited early (code $($proc.ExitCode)) — full log: $entLog" }
            throw "platform server never became ready on 18081 — full log: $entLog"
        }
        & (Join-Path $Server 'target\debug\mmp-conformance.exe') `
            --platform http://127.0.0.1:18081 --rw-token ent-rw --mgmt-token ent-mgmt
        if ($LASTEXITCODE -ne 0) {
            Write-Host '--- platform server stderr (tail) ---' -ForegroundColor Yellow
            Get-Content $entLog -Tail 40 -ErrorAction SilentlyContinue | Write-Host
        }
    }
    finally {
        Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
        'MUNARIUM_HTTP_ADDR','MUNARIUM_GRPC_ADDR','MUNARIUM_OPS_ADDR','MUNARIUM_STORE',
        'MUNARIUM_SOURCE_STORE','MUNARIUM_DATABASE_URL','MUNARIUM_AUTH_MODE',
        'MUNARIUM_STATIC_TOKENS','MUNARIUM_TOKEN_SECRET' |
            ForEach-Object { Remove-Item "env:$_" -ErrorAction SilentlyContinue }
    }
}

Step 'conformance (cluster, two instances, pg-backed)' {
    # Mirrors server-ci.yml 'conformance (cluster ...)' and test.ps1
    # -Cluster — change all three in the same commit. Two instances of
    # the same binary against the same database prove the N-replica
    # mechanics (registry convergence, shared idempotency, seq
    # interleaving, the run advisory lock). Binaries were built above.
    foreach ($port in 18082, 18083, 19092, 19093) {
        $conn = Get-NetTCPConnection -State Listen -LocalPort $port -ErrorAction SilentlyContinue
        if ($conn) {
            $ownerPid = $conn[0].OwningProcess
            $owner = (Get-Process -Id $ownerPid -ErrorAction SilentlyContinue).ProcessName
            if ($owner -eq 'munarium-server') {
                Write-Host "  reaping leftover munarium-server (pid $ownerPid) on port $port" -ForegroundColor DarkGray
                Stop-Process -Id $ownerPid -Force -ErrorAction SilentlyContinue
                Start-Sleep -Milliseconds 500
            }
            else {
                throw "port $port is already in use by pid $ownerPid ($owner) — not an munarium-server, so not stopping it for you"
            }
        }
    }
    $clTenant = "cl-gate-$(Get-Random)"
    $env:MUNARIUM_GRPC_ADDR = 'disabled'
    $env:MUNARIUM_STORE = 'postgres'
    $env:MUNARIUM_SOURCE_STORE = 'pg'
    $env:MUNARIUM_DATABASE_URL = $TestDbUrl
    $env:MUNARIUM_AUTH_MODE = 'static'
    $env:MUNARIUM_STATIC_TOKENS = "cl-rw:${clTenant}:rw"
    $env:MUNARIUM_REGISTRY_TTL_SECS = '1'
    $env:MUNARIUM_REPLICA_COUNT = '2'
    $procs = @()
    try {
        foreach ($inst in @(
                @{ Id = 'gate-a'; Http = '127.0.0.1:18082'; Ops = '127.0.0.1:19092' },
                @{ Id = 'gate-b'; Http = '127.0.0.1:18083'; Ops = '127.0.0.1:19093' }
            )) {
            $env:MUNARIUM_HTTP_ADDR = $inst.Http
            $env:MUNARIUM_OPS_ADDR = $inst.Ops
            $env:MUNARIUM_INSTANCE_ID = $inst.Id
            $procs += Start-Process -FilePath (Join-Path $Server 'target\debug\munarium-server.exe') `
                -WorkingDirectory $Server -PassThru -NoNewWindow `
                -RedirectStandardError (Join-Path ([IO.Path]::GetTempPath()) "munarium-$($inst.Id).err.log")
        }
        $ready = $false
        foreach ($i in 1..30) {
            try {
                Invoke-RestMethod 'http://127.0.0.1:18082/healthz' | Out-Null
                Invoke-RestMethod 'http://127.0.0.1:18083/healthz' | Out-Null
                $ready = $true; break
            }
            catch { Start-Sleep 1 }
        }
        if (-not $ready) { throw 'cluster servers never became ready on 18082/18083' }
        & (Join-Path $Server 'target\debug\mmp-conformance.exe') `
            --cluster http://127.0.0.1:18082 --peer http://127.0.0.1:18083 --token cl-rw
    }
    finally {
        foreach ($p in $procs) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
        'MUNARIUM_HTTP_ADDR','MUNARIUM_OPS_ADDR','MUNARIUM_INSTANCE_ID','MUNARIUM_GRPC_ADDR',
        'MUNARIUM_STORE','MUNARIUM_SOURCE_STORE','MUNARIUM_DATABASE_URL','MUNARIUM_AUTH_MODE',
        'MUNARIUM_STATIC_TOKENS','MUNARIUM_REGISTRY_TTL_SECS','MUNARIUM_REPLICA_COUNT' |
            ForEach-Object { Remove-Item "env:$_" -ErrorAction SilentlyContinue }
    }
}

Step 'openapi drift check' {
    $tmp = Join-Path ([IO.Path]::GetTempPath()) 'munarium-openapi-local.json'
    cargo run -q -p munarium-server -- openapi > $tmp
    if ($LASTEXITCODE -ne 0) { return }
    py -c "import json,sys; a=json.load(open(sys.argv[1],encoding='utf-8-sig')); b=json.load(open(sys.argv[2],encoding='utf-8-sig')); sys.exit(0 if a==b else 'docs/api/openapi.json is stale: regenerate with cargo run -p munarium-server -- openapi')" `
        $tmp (Join-Path $Server 'docs\api\openapi.json')
}

Step 'contract bundle self-test' {
    # Mirrored in .github/workflows/server-ci.yml 'contract bundle is
    # reproducible' — change both in the same commit. Two cuts of the public
    # MMP contract bundle must be byte-identical and match the lock.
    py (Join-Path $Server 'contract\mmp\publish.py') --self-test
}

# The two steps below read sibling trees: server/, matrix/ and clients/ are one
# repository, so a checkout that has one has all of them.
Step 'matrix contract drift check' {
    # Mirrored in server-ci.yml and matrix-ci.yml — change all three in the
    # same commit. server/contract/matrix is cut from matrix/contract by its
    # publisher, lock included; re-cut with:
    #   rm -r server/contract/matrix; py matrix/contract/publish.py --out server/contract/matrix
    # munarium-api-types' matrix_contract lock test, which cargo test runs
    # above, verifies the vendored bytes against the lock independently.
    py (Join-Path $Server '..\matrix\contract\publish.py') --check (Join-Path $Server 'contract\matrix')
}

Step 'license gate (manifests, SPDX headers, license texts)' {
    # Mirrored in server-ci.yml — change both in the same commit. The licence gate:
    # Apache-2.0 in the workspace and every crate, the license
    # texts present, an SPDX line on every source file that can carry one
    # (`py check_license.py --stamp` adds a missing header).
    py (Join-Path $Server 'check_license.py')
}

Step 'third-party notices current for the shipping crate graph' {
    # Section 6.5: THIRD_PARTY_NOTICES.md is generated from the musl-resolved
    # runtime graph and reviewed; a dependency change that is not regenerated
    # fails here. Regenerate: py tools/third_party_notices.py --cargo-target x86_64-unknown-linux-musl
    py (Join-Path $Server 'tools\third_party_notices.py') --check --cargo-target x86_64-unknown-linux-musl
}

Step 'grpc-reference drift check' {
    $tmp = Join-Path ([IO.Path]::GetTempPath()) 'munarium-grpc-reference-local.md'
    cargo run -q -p munarium-proto --bin gen-grpc-docs -- $tmp
    if ($LASTEXITCODE -ne 0) { return }
    git diff --no-index --exit-code -- (Join-Path $Server 'docs\api\grpc-reference.md') $tmp
    if ($LASTEXITCODE -ne 0) {
        throw 'docs/api/grpc-reference.md is stale: regenerate with cargo run -p munarium-proto --bin gen-grpc-docs -- docs/api/grpc-reference.md'
    }
}

Step 'crate boundary check (core + access purity; providers vs storage)' {
    # Mirrored in .github/workflows/server-ci.yml 'crate boundary check' —
    # change both in the same commit. Three machine-checked boundaries
    # (dev-guide §4 boundary table):
    #   munarium-core / munarium-access: never depend on sqlx/axum/tonic/reqwest/utoipa
    #   munarium-providers: never depends on a storage crate
    foreach ($crate in 'munarium-core', 'munarium-access') {
        $deps = cargo tree -p $crate -e normal --prefix none |
            ForEach-Object { ($_ -split '\s+')[0] } | Sort-Object -Unique
        foreach ($banned in 'sqlx', 'axum', 'tonic', 'reqwest', 'utoipa') {
            if ($deps -contains $banned) { throw "BOUNDARY VIOLATION: $crate depends on $banned" }
        }
    }
    $deps = cargo tree -p munarium-providers -e normal --prefix none |
        ForEach-Object { ($_ -split '\s+')[0] } | Sort-Object -Unique
    foreach ($banned in 'munarium-store-pg', 'munarium-store-mem', 'munarium-retrieval-pg') {
        if ($deps -contains $banned) { throw "BOUNDARY VIOLATION: munarium-providers depends on $banned" }
    }
    # munarium-api-types ships in the public contract bundle: it may depend on munarium-proto
    # and on nothing else of the workspace. The core <-> DTO conversions live
    # in munarium-api-conv.
    $deps = cargo tree -p munarium-api-types --all-features -e normal --prefix none |
        ForEach-Object { ($_ -split '\s+')[0] } | Sort-Object -Unique
    $strays = @($deps | Where-Object { $_ -like 'munarium-*' -and $_ -notin @('munarium-api-types', 'munarium-proto') })
    if ($strays.Count -gt 0) { throw "BOUNDARY VIOLATION: munarium-api-types depends on a server crate: $($strays -join ', ')" }
    $global:LASTEXITCODE = 0
}

Step 'retrieval boundary check (server names the coordinator, not the backend)' {
    # Mirrored in .github/workflows/server-ci.yml -- change both in the same commit.
    #
    # Cargo has no notion of "a dependency only the composition root may
    # name", so the retrieval boundary is enforced at source level:
    # munarium-server may reference munarium_retrieval_pg ONLY in state.rs,
    # where AppState constructs PgRetrieval and PgSourceStore. Without this
    # the extraction silently rots -- one `use munarium_retrieval_pg::` in a
    # new handler and PostgreSQL is the real interface again.
    $strays = Select-String -Path 'src/munarium-server/src/*.rs' -Pattern 'munarium_retrieval_pg' -List |
        Where-Object { $_.Filename -ne 'state.rs' } | ForEach-Object { $_.Filename }
    if ($strays) {
        throw "BOUNDARY VIOLATION: munarium-server names munarium_retrieval_pg outside state.rs: $($strays -join ', ')"
    }
    $global:LASTEXITCODE = 0
}

Step 'datastore boundary check (the crate stays independently usable)' {
    # Mirrored in .github/workflows/server-ci.yml -- change both in the same commit.
    #
    # munarium-datastore must keep no Axum, tonic, SQLx, PostgreSQL, server
    # config, auth or runbooks in its graph, and no munarium-core either:
    # the premise is a crate that can be lifted out of this
    # workspace. A boundary nothing enforces lasts until the first
    # convenient import.
    $deps = cargo tree -p munarium-datastore -e normal --prefix none |
        ForEach-Object { ($_ -split '\s+')[0] } | Sort-Object -Unique
    foreach ($banned in 'sqlx', 'axum', 'tonic', 'reqwest', 'utoipa', 'munarium-core',
                        'munarium-store-pg', 'munarium-retrieval-pg', 'munarium-server',
                        'munarium-runbooks') {
        if ($deps -contains $banned) { throw "BOUNDARY VIOLATION: munarium-datastore depends on $banned" }
    }
    $global:LASTEXITCODE = 0
}

Step 'additive-only migrations check' {
    $bad = Get-ChildItem (Join-Path $Server 'src\munarium-store-pg\migrations') -File -Recurse |
        Select-String -Pattern '^\s*(drop\s+(table|column)|alter\s+table\s+\S+\s+drop)'
    if ($bad) { $bad | Format-Table; throw 'DESTRUCTIVE DDL in migrations' }
    $global:LASTEXITCODE = 0
}

# ---- the cargo-deny job (optional locally; CI still gates push/PR) -----
if (Get-Command cargo-deny -ErrorAction SilentlyContinue) {
    Step 'cargo deny (licenses/advisories)' { cargo deny check }
}
else {
    Write-Warning 'cargo-deny not installed — skipping the license/advisory gate (cargo install cargo-deny --locked). CI still enforces it.'
}

Write-Host "`nALL LOCAL GATES PASSED" -ForegroundColor Green
}
finally { Pop-Location }
