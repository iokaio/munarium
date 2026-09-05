# SPDX-License-Identifier: Apache-2.0
# localdeploy.ps1 — build and run the full munarium stack on local Docker Desktop:
# the munarium-server container + a PostgreSQL (pgvector) container with
# PERSISTENT storage (named volumes server_pgdata / server_objects survive
# restarts and redeploys). No Azure involved.
#
#   .\localdeploy.ps1                 # build + deploy server & postgres
#   .\localdeploy.ps1 -Gateway       # + the Envoy gateway plane container
#   .\localdeploy.ps1 -NoBuild       # redeploy without rebuilding the image
#   .\localdeploy.ps1 -Down          # stop the stack (volumes/data KEPT)
#   .\localdeploy.ps1 -Down -Purge   # stop AND delete the persistent volumes
#
# Default host ports avoid this machine's conflicts (8080 in use, 8443
# Windows-reserved). Override with -HttpPort/-GrpcPort/-OpsPort/-GatewayPort.

param(
    [switch]$Gateway,
    [switch]$NoBuild,
    [switch]$Down,
    [switch]$Purge,
    [int]$HttpPort = 18080,
    [int]$GrpcPort = 15051,
    [int]$OpsPort = 19090,
    [int]$GatewayPort = 18443
)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

$env:MUNARIUM_HOST_HTTP = $HttpPort
$env:MUNARIUM_HOST_GRPC = $GrpcPort
$env:MUNARIUM_HOST_OPS = $OpsPort
$env:MUNARIUM_HOST_GATEWAY = $GatewayPort
$composeArgs = @('--profile', 'gateway')   # include profile so Down covers envoy too

try {
    if ($Down) {
        Write-Host '== stopping local stack' -ForegroundColor Cyan
        if ($Purge) {
            docker compose @composeArgs down -v
            Write-Host 'stack stopped; persistent volumes DELETED' -ForegroundColor Yellow
        } else {
            docker compose @composeArgs down
            Write-Host 'stack stopped; data kept in volumes server_pgdata / server_objects' -ForegroundColor Green
        }
        return
    }

    if (-not $NoBuild) {
        Write-Host '== building munarium-server image (musl -> distroless)' -ForegroundColor Cyan
        docker compose build munarium-server
        if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    }

    $services = @('postgres', 'munarium-server')
    Write-Host "== deploying: $($services -join ', ')$(if ($Gateway) { ', envoy' })" -ForegroundColor Cyan
    if ($Gateway) {
        docker compose --profile gateway up -d
    } else {
        docker compose up -d @services
    }
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

    # wait for the server to answer (postgres health is a compose dependency)
    $deadline = (Get-Date).AddSeconds(90)
    do {
        Start-Sleep -Seconds 2
        try { $ok = (Invoke-WebRequest "http://localhost:$HttpPort/healthz" -TimeoutSec 3).StatusCode -eq 200 }
        catch { $ok = $false }
    } until ($ok -or (Get-Date) -gt $deadline)
    if (-not $ok) {
        docker compose logs munarium-server --tail 15
        Write-Error "server did not answer on http://localhost:$HttpPort/healthz"
    }

    Write-Host ''
    Write-Host 'LOCAL STACK UP' -ForegroundColor Green
    Write-Host "  REST          http://localhost:$HttpPort        (docs: /docs, spec: /openapi.json)"
    Write-Host "  direct gRPC   localhost:$GrpcPort               (plaintext; grpcurl -plaintext ... list)"
    if ($Gateway) {
        Write-Host "  gateway       http://localhost:$GatewayPort  (REST + gRPC on one port, content-type routed)"
    }
    Write-Host "  postgres      localhost:5433 (postgres://munarium:munarium-dev@localhost:5433/munarium)"
    Write-Host "  auth          Authorization: Bearer devtoken    (tenant dev-tenant, rw)"
    Write-Host "  data          persisted in volumes server_pgdata / server_objects"
    Write-Host ''
    Write-Host 'try it:'
    Write-Host "  curl http://localhost:$HttpPort/version"
    Write-Host "  cargo run -p mmp-conformance -- --http http://localhost:$HttpPort --grpc http://localhost:$GrpcPort --token devtoken"
} finally {
    'MUNARIUM_HOST_HTTP', 'MUNARIUM_HOST_GRPC', 'MUNARIUM_HOST_OPS', 'MUNARIUM_HOST_GATEWAY' |
        ForEach-Object { Remove-Item "Env:$_" -ErrorAction SilentlyContinue }
}
