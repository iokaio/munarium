# SPDX-License-Identifier: Apache-2.0
#Requires -Version 7
[CmdletBinding()]
param([string]$ReceiptPath)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/validation.ps1"
$root = Split-Path (Split-Path $PSScriptRoot)
$inputs = @(
    'server/tools/test-datastore-permissions.ps1',
    'server/tools/datastore-permissions.Dockerfile',
    'server/src/munarium-datastore/tests/support/restricted_filesystem.rs'
)
New-ValidationRun 'datastore-linux-permissions' $root $ReceiptPath -Inputs $inputs -NotRequested @('windows-appcontainer')
$image = 'munarium-permissions-' + $script:Validation.Receipt.run_id
Add-ValidationStep 'linux.available' {
    $os = Invoke-ValidationCommand docker @('version','--format','{{.Server.Os}}') -Capture -AcceptExitCodes @(0,1)
    if ($os -notcontains 'linux') { throw 'missing_image' }
} -Requires docker
Add-ValidationStep 'linux.build' {
    # A tracked-file allowlist plus the explicit new fixture: no ignored local
    # secrets, artifacts, or environment files enter Docker's build context.
    $context = Join-Path $script:Validation.Directory 'context'
    [void][IO.Directory]::CreateDirectory($context)
    $paths = Invoke-ValidationCommand git @('-C',$root,'ls-files','server') -Capture
    foreach ($path in @($paths + $inputs | Sort-Object -Unique)) {
        if ($path -notmatch '^server/(Cargo\.(toml|lock)$|src/|conformance/|contract/|runbooks/|tools/datastore-permissions\.Dockerfile$)') { continue }
        $destination = Join-Path $context $path.Substring(7)
        [void][IO.Directory]::CreateDirectory((Split-Path $destination))
        Copy-Item -LiteralPath (Join-Path $root $path) -Destination $destination
    }
    Invoke-ValidationCommand docker @('build','--file',"$context/tools/datastore-permissions.Dockerfile",'--tag',$image,$context)
} -Requires docker -DependsOn 'linux.available'
$validation = $script:Validation
foreach ($case in 'serve','missing-scratch','readonly-scratch','denied-traversal','corrupt') {
    $body = {
        $validation.Container = $image + '-' + $case
        $scratch = switch ($case) { 'missing-scratch' { '/missing/scratch' } 'readonly-scratch' { '/qualification' } default { '/scratch' } }
        try {
            Invoke-ValidationCommand docker @('run','--name',$validation.Container,'--network','none',
                '--read-only','--cap-drop','ALL','--security-opt','no-new-privileges',
                '--user','65532:65532','--pids-limit','128','--memory','512m',
                '--tmpfs','/scratch:rw,noexec,nosuid,nodev,size=64m,uid=65532,gid=65532,mode=0700',
                '--env',"TMPDIR=$scratch",'--env',"PERMISSIONS_CASE=$case",$image)
        } finally {
            Invoke-ValidationCommand docker @('rm','-f',$validation.Container)
            $validation.Container = $null
        }
    }.GetNewClosure()
    Add-ValidationStep "linux.$case" $body -Requires docker -DependsOn 'linux.build'
}
Add-ValidationStep 'linux.cleanup-image' {
    $present = Invoke-ValidationCommand docker @('image','ls','--quiet',$image) -Capture
    if ($present) { Invoke-ValidationCommand docker @('image','rm',$image) }
} -Requires docker -DependsOn 'linux.available'
Add-ValidationStep 'linux.cleanup-context' {
    $context = [IO.Path]::GetFullPath((Join-Path $script:Validation.Directory 'context'))
    $ownedRoot = [IO.Path]::GetFullPath($script:Validation.Directory).TrimEnd('/','\') + [IO.Path]::DirectorySeparatorChar
    if (-not $context.StartsWith($ownedRoot) -or (Split-Path $context -Leaf) -ne 'context') { throw 'unsafe_cleanup_path' }
    if (Test-Path -LiteralPath $context) { Remove-Item -LiteralPath $context -Recurse -Force }
}
$code = Invoke-ValidationRun
exit $code
