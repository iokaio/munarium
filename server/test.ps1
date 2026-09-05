# SPDX-License-Identifier: Apache-2.0
# test.ps1 — run the munarium-server test tiers.
#
#   .\test.ps1               # offline: workspace unit/integration tests + in-process conformance
#   .\test.ps1 -Postgres     # + pg-backed tests and conformance (starts compose postgres if needed)
#   .\test.ps1 -BlackBox     # + live server, conformance over BOTH API planes (the parity check)
#   .\test.ps1 -Platform   # + pg-backed live server, the platform scenarios
#                            #   (uid contract, tokens, runbook v2, sessions, ingest, removal, reports)
#   .\test.ps1 -Cluster      # + TWO pg-backed live servers sharing one database — the
#                            #   N-replica scenarios (registry convergence, shared
#                            #   idempotency, interleaved seq, run advisory lock)
#   .\test.ps1 -All          # everything above
#
# The pg tiers use the compose postgres on localhost:5433 (pgvector/pgvector:pg16).
# Black-box uses ports 18080/15051/19090; platform 18081/19091; cluster
# 18082/19092 + 18083/19093 (gRPC disabled in the cluster tier — its hazards
# are transport-independent).
# Exit code is non-zero on the first failing tier.

param(
    [switch]$Postgres,
    [switch]$BlackBox,
    [switch]$Platform,
    [switch]$Cluster,
    [switch]$All
)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot
if ($All) { $Postgres = $true; $BlackBox = $true; $Platform = $true; $Cluster = $true }

$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) { $cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (-not (Test-Path $cargo)) { Write-Error 'cargo not found — install Rust via winget install Rustlang.Rustup' }

$pgUrl = 'postgres://munarium:munarium-dev@localhost:5433/munarium'

function Ensure-Postgres {
    Write-Host '== ensuring compose postgres is up' -ForegroundColor Cyan
    docker compose up -d postgres 2>&1 | Out-Null   # idempotent
    # wait for the container healthcheck (pg_isready)
    $deadline = (Get-Date).AddSeconds(90)
    do {
        Start-Sleep -Seconds 2
        $health = (docker inspect --format '{{.State.Health.Status}}' server-postgres-1 2>$null) -join ''
    } until ($health -eq 'healthy' -or (Get-Date) -gt $deadline)
    if ($health -ne 'healthy') {
        docker compose logs postgres --tail 10
        Write-Error "postgres did not become healthy in 90s (state: '$health')"
    }
}

# --- tier 1: offline tests + in-process conformance ---------------------------
if ($Postgres) { Ensure-Postgres; $env:MUNARIUM_TEST_DATABASE_URL = $pgUrl }
try {
    Write-Host '== cargo test --workspace' -ForegroundColor Cyan
    & $cargo test --workspace
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    Remove-Item Env:MUNARIUM_TEST_DATABASE_URL -ErrorAction SilentlyContinue
}

Write-Host '== conformance: in-process (munarium-store-mem)' -ForegroundColor Cyan
& $cargo run -q -p mmp-conformance -- --in-process
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# --- tier 2: pg-backed conformance ------------------------------------------
if ($Postgres) {
    Write-Host '== conformance: postgres (munarium-store-pg)' -ForegroundColor Cyan
    & $cargo run -q -p mmp-conformance -- --postgres $pgUrl
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

# --- tier 3: black-box over both API planes -----------------------------------
if ($BlackBox) {
    Write-Host '== black-box: starting server (memory store, 18080/15051)' -ForegroundColor Cyan
    & $cargo build -q -p munarium-server
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    $token = "bbtoken"
    $tenant = "bb-$(Get-Random)"
    $serverEnv = @{
        MUNARIUM_HTTP_ADDR     = '127.0.0.1:18080'
        MUNARIUM_GRPC_ADDR     = '127.0.0.1:15051'
        MUNARIUM_OPS_ADDR      = '127.0.0.1:19090'
        MUNARIUM_STORE         = 'memory'
        MUNARIUM_AUTH_MODE     = 'static'
        MUNARIUM_STATIC_TOKENS = "${token}:${tenant}:rw"
    }
    foreach ($k in $serverEnv.Keys) { Set-Item "Env:$k" $serverEnv[$k] }
    $server = Start-Process -FilePath (Join-Path $PSScriptRoot 'target\debug\munarium-server.exe') `
        -PassThru -NoNewWindow -RedirectStandardError (Join-Path $env:TEMP 'munarium-test-server.log')
    try {
        $deadline = (Get-Date).AddSeconds(30)
        do {
            Start-Sleep -Seconds 1
            try { $ok = (Invoke-WebRequest 'http://127.0.0.1:18080/healthz' -TimeoutSec 3).StatusCode -eq 200 }
            catch { $ok = $false }
        } until ($ok -or (Get-Date) -gt $deadline)
        if (-not $ok) {
            Get-Content (Join-Path $env:TEMP 'munarium-test-server.log') -Tail 10
            Write-Error 'server did not come up on 18080'
        }
        Write-Host '== conformance: REST + gRPC planes (parity)' -ForegroundColor Cyan
        & $cargo run -q -p mmp-conformance -- `
            --http http://127.0.0.1:18080 --grpc http://127.0.0.1:15051 --token $token
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    } finally {
        Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
        foreach ($k in $serverEnv.Keys) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue }
    }
}

# --- tier 4: platform surface over a pg-backed live server ---------
if ($Platform) {
    Ensure-Postgres
    Write-Host '== platform: starting server (postgres store, 18081)' -ForegroundColor Cyan
    & $cargo build -q -p munarium-server -p mmp-conformance
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    # Fresh tenant per run so scenarios never collide with earlier data.
    $entTenant = "ent-$(Get-Random)"
    $serverEnv = @{
        MUNARIUM_HTTP_ADDR    = '127.0.0.1:18081'
        MUNARIUM_GRPC_ADDR    = 'disabled'
        MUNARIUM_OPS_ADDR     = '127.0.0.1:19091'
        MUNARIUM_STORE        = 'postgres'
        MUNARIUM_DATABASE_URL = $pgUrl
        # Under MUNARIUM_STORE=postgres the source store DEFAULTS to 'az' and
        # fails closed without a storage account — on a clean env the server
        # exits before /healthz ever answers. Pin pg, exactly like CI's
        # platform step (server-ci.yml) and the black-box gates do.
        MUNARIUM_SOURCE_STORE = 'pg'
        MUNARIUM_AUTH_MODE    = 'static'
        MUNARIUM_STATIC_TOKENS = "ent-rw:${entTenant}:rw,ent-mgmt:${entTenant}:mgmt"
        MUNARIUM_TOKEN_SECRET = 'test-tier-platform-secret-32-bytes!!'
    }
    foreach ($k in $serverEnv.Keys) { Set-Item "Env:$k" $serverEnv[$k] }
    $server = Start-Process -FilePath (Join-Path $PSScriptRoot 'target\debug\munarium-server.exe') `
        -PassThru -NoNewWindow -RedirectStandardError (Join-Path $env:TEMP 'munarium-ent-server.log')
    try {
        $deadline = (Get-Date).AddSeconds(30)
        do {
            Start-Sleep -Seconds 1
            try { $ok = (Invoke-WebRequest 'http://127.0.0.1:18081/healthz' -TimeoutSec 3).StatusCode -eq 200 }
            catch { $ok = $false }
        } until ($ok -or (Get-Date) -gt $deadline)
        if (-not $ok) {
            Get-Content (Join-Path $env:TEMP 'munarium-ent-server.log') -Tail 10
            Write-Error 'platform server did not come up on 18081'
        }
        Write-Host '== conformance: platform scenarios (uid/tokens/runbook-v2/sessions/ingest/removal/reports)' -ForegroundColor Cyan
        & $cargo run -q -p mmp-conformance -- `
            --platform http://127.0.0.1:18081 --rw-token ent-rw --mgmt-token ent-mgmt
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    } finally {
        Stop-Process -Id $server.Id -Force -ErrorAction SilentlyContinue
        foreach ($k in $serverEnv.Keys) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue }
    }
}

# --- tier 5: N-replica cluster scenarios (2026-08-17) -------------------------
# Two instances of the SAME binary against the SAME database and tenant —
# the black-box proof of the clustering work: registry convergence within
# MUNARIUM_REGISTRY_TTL_SECS, table-backed idempotency across instances,
# interleaved seq allocation, and the runbook run advisory lock. Mirrors the
# environment contract in conformance/src/cluster.rs.
if ($Cluster) {
    Ensure-Postgres
    Write-Host '== cluster: starting two servers (postgres store, 18082 + 18083)' -ForegroundColor Cyan
    & $cargo build -q -p munarium-server -p mmp-conformance
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    # Fresh tenant per run, shared by BOTH instances.
    $clTenant = "cl-$(Get-Random)"
    $sharedEnv = @{
        MUNARIUM_GRPC_ADDR         = 'disabled'
        MUNARIUM_STORE             = 'postgres'
        MUNARIUM_DATABASE_URL      = $pgUrl
        MUNARIUM_SOURCE_STORE      = 'pg'
        MUNARIUM_AUTH_MODE         = 'static'
        MUNARIUM_STATIC_TOKENS     = "cl-rw:${clTenant}:rw"
        # 1s so the convergence scenarios wait ~1.5s instead of the 15s default.
        MUNARIUM_REGISTRY_TTL_SECS = '1'
        MUNARIUM_REPLICA_COUNT     = '2'
    }
    $servers = @()
    try {
        foreach ($inst in @(
                @{ Id = 'cl-a'; Http = '127.0.0.1:18082'; Ops = '127.0.0.1:19092' },
                @{ Id = 'cl-b'; Http = '127.0.0.1:18083'; Ops = '127.0.0.1:19093' }
            )) {
            foreach ($k in $sharedEnv.Keys) { Set-Item "Env:$k" $sharedEnv[$k] }
            $env:MUNARIUM_HTTP_ADDR = $inst.Http
            $env:MUNARIUM_OPS_ADDR = $inst.Ops
            $env:MUNARIUM_INSTANCE_ID = $inst.Id
            $servers += Start-Process -FilePath (Join-Path $PSScriptRoot 'target\debug\munarium-server.exe') `
                -PassThru -NoNewWindow -RedirectStandardError (Join-Path $env:TEMP "munarium-$($inst.Id)-server.log")
        }
        foreach ($probe in 'http://127.0.0.1:18082/healthz', 'http://127.0.0.1:18083/healthz') {
            $deadline = (Get-Date).AddSeconds(30)
            do {
                Start-Sleep -Seconds 1
                try { $ok = (Invoke-WebRequest $probe -TimeoutSec 3).StatusCode -eq 200 }
                catch { $ok = $false }
            } until ($ok -or (Get-Date) -gt $deadline)
            if (-not $ok) {
                Get-Content (Join-Path $env:TEMP 'munarium-cl-a-server.log') -Tail 10 -ErrorAction SilentlyContinue
                Get-Content (Join-Path $env:TEMP 'munarium-cl-b-server.log') -Tail 10 -ErrorAction SilentlyContinue
                Write-Error "cluster server did not come up ($probe)"
            }
        }
        Write-Host '== conformance: cluster scenarios (registry/idempotency/seq/run-lock across two instances)' -ForegroundColor Cyan
        & $cargo run -q -p mmp-conformance -- `
            --cluster http://127.0.0.1:18082 --peer http://127.0.0.1:18083 --token cl-rw
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    } finally {
        foreach ($s in $servers) { Stop-Process -Id $s.Id -Force -ErrorAction SilentlyContinue }
        foreach ($k in $sharedEnv.Keys) { Remove-Item "Env:$k" -ErrorAction SilentlyContinue }
        foreach ($k in 'MUNARIUM_HTTP_ADDR', 'MUNARIUM_OPS_ADDR', 'MUNARIUM_INSTANCE_ID') {
            Remove-Item "Env:$k" -ErrorAction SilentlyContinue
        }
    }
}

Write-Host 'all requested test tiers OK' -ForegroundColor Green
