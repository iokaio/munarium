# SPDX-License-Identifier: Apache-2.0
# Qualify munarium-datastore's supported embedded tier from its isolated consumer
# fixture (P15/D5). Writes a validation receipt; exit 0 passed, 1 failed, 3 when
# required coverage could not run (for example, the declared minimum compiler is
# not installed: `rustup toolchain install <rust-version> --profile minimal`).
#Requires -Version 7
[CmdletBinding()]
param([string]$ReceiptPath)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/validation.ps1"
. "$PSScriptRoot/embedded-datastore.ps1"
Push-Location (Split-Path $PSScriptRoot)
try {
    $root = Split-Path (Get-Location).Path
    New-ValidationRun 'embedded-datastore' $root $ReceiptPath -Inputs @(
        'server/tools/embedded-datastore.ps1', 'server/tools/test-embedded-datastore.ps1',
        'server/tests/embedded-datastore/Cargo.toml', 'server/tests/embedded-datastore/Cargo.lock',
        'server/tests/embedded-datastore/src/lib.rs', 'server/tests/embedded-datastore/tests/datastore_suite.rs',
        'server/src/munarium-datastore/Cargo.toml'
    )
    Add-EmbeddedDatastoreSteps
    $code = Invoke-ValidationRun
} catch { Write-Error 'Embedded datastore qualification could not initialize.' -ErrorAction Continue; $code = 1 }
finally { Pop-Location }
exit $code
