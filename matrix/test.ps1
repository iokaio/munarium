# SPDX-License-Identifier: Apache-2.0
<#
.SYNOPSIS
  The Matrix test runner. Four tiers, the same vocabulary the plan uses.

.DESCRIPTION
  Default (no switch) is the OFFLINE tier: workspace unit tests plus the
  boundary checks, no database, no network, $0. That is what runs on every
  save and what CI runs on every push.

    ./test.ps1                 offline: unit tests + boundary + contract checks
    ./test.ps1 -Postgres       + store and conformance tests against compose Postgres
    ./test.ps1 -BlackBox       + a compose-launched Matrix, conformance over HTTP
                               (incl. gRPC, MCP and the /admin console tier),
                               and the sqlserver engine tier
    ./test.ps1 -Gates          fmt + clippy -D warnings + the offline tier
    ./test.ps1 -All            everything the laptop can run
  Prereqs: Rust stable with rustfmt and clippy, Docker for the compose tiers,
  and a Python 3 for the helper steps -- on Windows the py launcher, which is
  what server/gates.ps1 uses too (see the interpreter probe below).

  The HTTP tier needs a compose-launched SERVER too, because sealing evidence
  needs a peer and a role that seals cannot run without one. Two profiles
  provide it: `server-source` builds THIS checkout's ../server and needs no
  registry, which is what matrix-server-contract.yml tests on a pull request;
  `server` pulls a published image instead and needs MUNARIUM_SERVER_IMAGE set:

    $env:MUNARIUM_MATRIX_COMPOSE_SERVER_URL = "http://munarium-server:8080"
    docker compose --profile server-source up -d --build  # builds ../server

  Engine tiers that compose CANNOT stand up are not here either, and they say
  so rather than passing quietly: mysql (`--profile mysql`), cube
  (`--profile cube`), snowflake and bigquery each need a variable set, and
  every scenario in an unset tier prints SKIPPED. Snowflake and BigQuery have
  no account at all — see docs/adapters/build-matrix.md, which records exactly
  what is proven about each adapter and what is not.
#>
[CmdletBinding()]
param(
    [switch]$Postgres,
    [switch]$BlackBox,
    [switch]$Gates,
    # The operator console through a real browser (Playwright, Node, dev-only;
    # matrix/ui-smoke). Needs -BlackBox for a console to drive. Produces the
    # screenshots docs/guides/admin-ui.md embeds.
    [switch]$Browser,
    [switch]$All
)

$ErrorActionPreference = 'Stop'
Set-Location $PSScriptRoot

# The interpreter for the Python helper steps, resolved once and by probing,
# because on Windows `python` is not necessarily an interpreter. The name is
# claimed first by the App Execution Alias in WindowsApps: a zero-byte stub
# that runs the Store build when one is installed, prints "Python was not
# found" (exit 9009) when none is, and cannot be launched at all from some
# sandboxes. server/gates.ps1 sidesteps it with the py launcher; this runner
# does the same, then falls back to the names CI uses on Linux, and refuses
# to start rather than let seven steps fail one at a time for one cause.
$python = $null
foreach ($candidate in 'py', 'python3', 'python') {
    if (-not (Get-Command $candidate -ErrorAction SilentlyContinue)) { continue }
    try { & $candidate --version *> $null } catch { continue }
    if ($LASTEXITCODE -eq 0) { $python = $candidate; break }
}
$global:LASTEXITCODE = 0
if (-not $python) {
    throw 'no working Python 3 found: install the py launcher (python.org build) or put python3/python on PATH'
}

if ($All) { $Postgres = $true; $BlackBox = $true; $Gates = $true }

$failures = New-Object System.Collections.Generic.List[string]
function Step {
    param([string]$Name, [scriptblock]$Body)
    Write-Host ""
    Write-Host "== $Name " -NoNewline -ForegroundColor Cyan
    Write-Host ("=" * [Math]::Max(0, 66 - $Name.Length)) -ForegroundColor DarkCyan
    $sw = [Diagnostics.Stopwatch]::StartNew()
    try {
        & $Body
        if ($LASTEXITCODE -ne 0 -and $null -ne $LASTEXITCODE) { throw "exit $LASTEXITCODE" }
        Write-Host "   ok  ($([int]$sw.Elapsed.TotalSeconds)s)" -ForegroundColor Green
    } catch {
        Write-Host "   FAILED: $_" -ForegroundColor Red
        $failures.Add($Name)
    }
}

# ---------------------------------------------------------------------------
# Gates: formatting and lints. Run these before asking anyone to review.
# ---------------------------------------------------------------------------
if ($Gates) {
    Step 'cargo fmt --check' { cargo fmt --all -- --check }
    Step 'cargo clippy -D warnings' { cargo clippy --workspace --all-targets -- -D warnings }
}

# ---------------------------------------------------------------------------
# Offline tier
# ---------------------------------------------------------------------------
Step 'unit tests (workspace)' { cargo test --workspace }

Step 'ground rules: dependency boundaries and additive migrations' {
    # Ground rules 1 and 3, plus the additive-migration rule. ONE
    # implementation, called identically by matrix-ci.yml, over the graph that
    # SHIPS (the musl target) rather than the host's -- two copies over two
    # targets is what kept CI red for three pushes while the laptop was green.
    & $python scripts/boundaries.py
}

Step 'contract examples validate against their schemas' {
    & $python contract/validate_examples.py
}

Step 'contract cuts reproducibly; the vendored copy matches (when present)' {
    # Mirrored in matrix-ci.yml and server-ci.yml -- change all three in the
    # same commit. server/contract/matrix is cut by contract/publish.py with a
    # contract.lock;
    # re-cut with:
    #   rm -r ../server/contract/matrix; python contract/publish.py --out ../server/contract/matrix
    & $python contract/publish.py --self-test
    & $python contract/publish.py --check ../server/contract/matrix
}

Step 'license gate (manifests, SPDX headers, license texts)' {
    # Mirrored in matrix-ci.yml's gates job -- change both in the same commit.
    # The license gate: Apache-2.0
    # in the workspace and every crate, the license texts present, an SPDX line on
    # every source file that can carry one (`py check_license.py --stamp` adds one).
    & $python check_license.py
}

Step 'third-party notices current for the shipping crate graph' {
    # Section 6.5: THIRD_PARTY_NOTICES.md is generated from the musl-resolved runtime
    # graph and reviewed; a dependency change that is not regenerated fails here.
    # Regenerate: python scripts/third_party_notices.py --cargo-target x86_64-unknown-linux-musl
    & $python scripts/third_party_notices.py --check --cargo-target x86_64-unknown-linux-musl
}

Step 'every cycle a document names has a results file (§18.3)' {
    # "A number quoted in any document of this set comes from a
    # conformance/results/<run-id>.json". The lint checks the half of that a
    # script can: every cycle id the documents name is either recorded or
    # declared unrecorded, with a reason, in conformance/results/UNRECORDED.
    & $python scripts/doclint.py
}

Step 'openapi parses and matches the committed copy' {
    # `docs/api/openapi.json` is COMMITTED so a customer can read the surface
    # without building the binary — and a committed generated file that nobody
    # regenerates is worse than none, because it describes a service that no
    # longer exists.
    #
    # The comparison runs INSIDE the binary (`openapi --check`) rather than
    # here. A shell is the wrong place for it: the document contains em-dashes,
    # and PowerShell's pipeline capture and `Get-Content` do not round-trip
    # UTF-8 the way a redirect writes it, so a text comparison in this script
    # reported drift between two files `cmp` calls identical.
    cargo run -q -p munarium-matrix-server --bin munarium-matrix -- openapi --check docs/api/openapi.json
}

# ---------------------------------------------------------------------------
# Compose tier
# ---------------------------------------------------------------------------
if ($Postgres -or $BlackBox) {
    Step 'compose postgres up' {
        docker compose up -d postgres
        $deadline = (Get-Date).AddSeconds(60)
        while ((Get-Date) -lt $deadline) {
            docker compose exec -T postgres pg_isready -U matrix *> $null
            if ($LASTEXITCODE -eq 0) { $global:LASTEXITCODE = 0; return }
            Start-Sleep -Seconds 1
        }
        throw 'postgres did not become ready'
    }

    Step 'compose postgres carries the CDC fixture' {
        # Postgres runs its init directory ONLY on an empty data dir, so a
        # volume created before `05-cdc-fixture.sql` existed will not have it —
        # and the cdc tier would then refuse `cdc_publication_missing`, which
        # reads like a defect in the adapter. Checked here so the message names
        # the actual problem. The cube fixture was mounted nowhere for weeks and
        # its scenarios passed on a hand-loaded database; this is that lesson.
        $pubs = docker compose exec -T postgres psql -U matrix -d matrix -tAc `
            "SELECT count(*) FROM pg_publication WHERE pubname LIKE 'munarium_matrix_%'"
        if ("$pubs".Trim() -eq '0') {
            throw 'the postgres volume predates the CDC fixture: run `docker compose down -v` once'
        }
        $wal = docker compose exec -T postgres psql -U matrix -d matrix -tAc 'SHOW wal_level'
        if ("$wal".Trim() -ne 'logical') {
            throw "wal_level is '$wal', not 'logical'; no replication slot can exist"
        }
        Write-Host "   publications present, wal_level=logical" -ForegroundColor DarkGray
    }
}

if ($Postgres) {
    Step 'store + conformance (postgres)' {
        $env:MUNARIUM_MATRIX_TEST_DATABASE_URL =
            'postgres://matrix_owner:matrix-owner-dev@127.0.0.1:5434/matrix'
        cargo test -p munarium-matrix-conformance -- --include-ignored
    }
}

# ---------------------------------------------------------------------------
# Engine tiers that compose can stand up for $0.
#
# These are separate profiles because each is a whole database image, and a
# developer running the offline tier should not pay for three of them. They are
# started by -BlackBox because that is the switch that already means "bring the
# world up".
#
# The variables below are what turn a tier from a printed SKIP into a run. A
# tier whose variable is unset says so out loud rather than returning early and
# printing `ok`, which is indistinguishable from having proved something.
# ---------------------------------------------------------------------------
if ($BlackBox) {
    Step 'compose sqlserver up' {
        docker compose --profile sqlserver up -d sqlserver
        # The healthcheck counts rows in the fixture's own table, so "healthy"
        # means the fixture loaded — not merely that the engine answers. SQL
        # Server takes tens of seconds to recover its databases on a cold
        # start, and the fixture is applied after that.
        $deadline = (Get-Date).AddSeconds(240)
        while ((Get-Date) -lt $deadline) {
            $state = (docker inspect --format '{{.State.Health.Status}}' matrix-sqlserver-1 2>$null)
            if ($state -eq 'healthy') { $global:LASTEXITCODE = 0; return }
            Start-Sleep -Seconds 3
        }
        throw 'sqlserver did not become healthy (the fixture may have failed to apply)'
    }
}

if ($BlackBox) {
    Step 'conformance over HTTP' {
        # The SQL Server tier, against the compose fixture. `matrix_reader` is
        # the row-level-secured login the fixture defines: it sees EMEA only,
        # so this tier measures a restricted view rather than a superuser's.
        $env:MUNARIUM_MATRIX_TEST_SQLSERVER =
            'Server=tcp:127.0.0.1,14330;User Id=matrix_reader;' +
            'Password=Matrix-Reader-Dev1!;Database=crm;TrustServerCertificate=true'
        # The mysql and cube tiers, which compose can stand up and which this
        # runner once never actually pointed at.
        # A profile that is running while its tier prints SKIPPED is the
        # vacuously-green failure in its purest form: the containers are up,
        # the suite is green, and nothing was tested. Guarded on the port
        # answering so a run without those profiles still skips loudly rather
        # than failing on a connection.
        if (Test-NetConnection -ComputerName 127.0.0.1 -Port 3307 -InformationLevel Quiet -WarningAction SilentlyContinue) {
            $env:MUNARIUM_MATRIX_TEST_MYSQL = 'mysql://matrix:matrix-dev@127.0.0.1:3307/crm'
        }
        $env:MUNARIUM_MATRIX_TEST_DATABASE_URL =
            'postgres://matrix_owner:matrix-owner-dev@127.0.0.1:5434/matrix'
        $env:MUNARIUM_MATRIX_TEST_HTTP = '1'
        # The gRPC plane, on the port compose publishes. The tenant is the one
        # the compose static token belongs to.
        # Honour the compose port override: on a Windows box Hyper-V reserves
        # port ranges, and 50151 sat inside one on the first dev machine this
        # ran on (`netsh interface ipv4 show excludedportrange protocol=tcp`).
        # Set MUNARIUM_MATRIX_HOST_GRPC to a free port and compose publishes
        # there; the container still listens on 50151.
        $grpcPort = if ($env:MUNARIUM_MATRIX_HOST_GRPC) { $env:MUNARIUM_MATRIX_HOST_GRPC } else { '50151' }
        $env:MUNARIUM_MATRIX_TEST_GRPC = "http://127.0.0.1:$grpcPort"
        $env:MUNARIUM_MATRIX_TEST_TENANT = 'tenant-default'
        cargo test -p munarium-matrix-conformance -- --include-ignored
    }
}

# ---------------------------------------------------------------------------
# The browser tier: Playwright drives the console the
# way an operator does — the login form, a real Origin on a write, the cookie
# — and produces the guide's screenshots. Node and a browser download are its
# own cost, which is why it is a switch and not part of -BlackBox.
# ---------------------------------------------------------------------------
if ($Browser) {
    Step 'operator console through a browser (ui-smoke)' {
        if (-not $BlackBox) { throw 'the browser tier needs -BlackBox for a console to drive' }
        Push-Location ui-smoke
        try {
            if (-not (Test-Path node_modules)) { npm install --no-audit --no-fund --loglevel=error }
            npx playwright install chromium
            $env:MUNARIUM_MATRIX_TEST_URL = 'http://127.0.0.1:8180'
            $env:MUNARIUM_MATRIX_TEST_MGMT_TOKEN = 'mxmgmt'
            $env:MUNARIUM_MATRIX_TEST_TOKEN = 'mxdev'
            node smoke.mjs
        } finally { Pop-Location }
    }
}

# ---------------------------------------------------------------------------
Write-Host ""
if ($failures.Count -gt 0) {
    Write-Host "FAILED: $($failures -join ', ')" -ForegroundColor Red
    exit 1
}
Write-Host "all green" -ForegroundColor Green
exit 0
