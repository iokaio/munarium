# SPDX-License-Identifier: Apache-2.0
# build.ps1 — build the munarium-server workspace.
#
#   .\build.ps1              # debug build of every crate
#   .\build.ps1 -Release     # optimized build
#   .\build.ps1 -Lint        # fmt --check + clippy -D warnings (what CI enforces)
#   .\build.ps1 -Image       # docker build of the distroless musl image
#   .\build.ps1 -Release -Lint -Image   # switches combine
#
# Exit code is non-zero on the first failing step.

param(
    [switch]$Release,
    [switch]$Lint,
    [switch]$Image,
    [string]$ImageTag = 'dev'
)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

# cargo: PATH first, rustup default location as fallback
$cargo = (Get-Command cargo -ErrorAction SilentlyContinue).Source
if (-not $cargo) { $cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (-not (Test-Path $cargo)) { Write-Error 'cargo not found — install Rust via winget install Rustlang.Rustup' }

if ($Lint) {
    Write-Host '== cargo fmt --check' -ForegroundColor Cyan
    & $cargo fmt --all --check
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    Write-Host '== cargo clippy -D warnings' -ForegroundColor Cyan
    & $cargo clippy --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

$profileArgs = @('build', '--workspace')
if ($Release) { $profileArgs += '--release' }
Write-Host "== cargo $($profileArgs -join ' ')" -ForegroundColor Cyan
& $cargo @profileArgs
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

if ($Image) {
    Write-Host "== docker build munarium-server:$ImageTag (musl -> distroless)" -ForegroundColor Cyan
    docker build -t "munarium-server:$ImageTag" .
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
    docker images "munarium-server:$ImageTag" --format 'built {{.Repository}}:{{.Tag}} ({{.Size}})'
}

Write-Host 'build OK' -ForegroundColor Green
