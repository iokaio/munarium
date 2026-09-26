# SPDX-License-Identifier: Apache-2.0
# Disposable PostgreSQL and live-server adapters for local validation.
. "$PSScriptRoot/gate-catalog.ps1"
function Start-ValidationPostgres {
    $image = 'pgvector/pgvector:pg16@sha256:ccc6e83d6e35e931dc7c5def2022729d5a6c370318d099181995567ff1fb4d6b'
    Invoke-ValidationCommand docker @('info','--format','{{.ServerVersion}}')
    $images = @(Invoke-ValidationCommand docker @('image','ls','--digests','--format','{{.Repository}}@{{.Digest}}','--filter','reference=pgvector/pgvector') -Capture)
    if (($image -replace ':pg16@','@') -notin $images) { throw 'missing_image' }
    $name = 'munarium-validation-' + $script:Validation.Receipt.run_id
    $id = (Invoke-ValidationCommand docker @('create','--name',$name,'--label',"munarium.validation=$name",'--publish','127.0.0.1::5432','-e','POSTGRES_USER=munarium','-e','POSTGRES_PASSWORD=munarium-test','-e','POSTGRES_DB=munarium',$image) -Capture) -join ''
    if ($id -notmatch '^[a-f0-9]{64}$') { throw 'malformed_output' }
    $script:Validation.Container = $id
    $script:Validation.Receipt.resources += [ordered]@{ kind = 'container'; id = $id; name = $name; cleanup = 'pending' }
    Save-ValidationReceipt
    Invoke-ValidationCommand docker @('start',$id)
    $binding = (Invoke-ValidationCommand docker @('port',$id,'5432/tcp') -Capture) -join ''
    if ($binding -notmatch '^127\.0\.0\.1:(\d+)$') { throw 'malformed_output' }
    $script:Validation.DatabaseUrl = "postgres://munarium:munarium-test@127.0.0.1:$($Matches[1])/munarium"
    foreach ($i in 1..60) {
        Invoke-ValidationCommand docker @('exec',$id,'pg_isready','-U','munarium') -Capture -AcceptExitCodes @(0,1,2) | Out-Null
        if ($script:Validation.Current.commands[-1].exit_code -eq 0) { return }
        Start-Sleep -Seconds 1
    }
    throw 'readiness_timeout'
}

function Get-ValidationPort {
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    try { $listener.Start(); return $listener.LocalEndpoint.Port } finally { $listener.Stop() }
}

function Start-ValidationServer {
    param([string]$Name, [hashtable]$Environment, [switch]$Grpc)
    $ports = @()
    while ($ports.Count -lt 3) { $port = Get-ValidationPort; if ($port -notin $ports) { $ports += $port } }
    $http = "http://127.0.0.1:$($ports[0])"
    $psi = [Diagnostics.ProcessStartInfo]::new()
    $suffix = if ($IsWindows) { '.exe' } else { '' }
    $psi.FileName = Join-Path $script:Validation.Target "debug/munarium-server$suffix"
    $psi.WorkingDirectory = $script:Validation.Directory
    $psi.UseShellExecute = $false; $psi.CreateNoWindow = $true
    # No operator provider credentials, remote stores or local configuration.
    foreach ($key in @($psi.Environment.Keys)) { if ($key -like 'MUNARIUM_*') { [void]$psi.Environment.Remove($key) } }
    foreach ($key in $Environment.Keys) { $psi.Environment[$key] = $Environment[$key] }
    $psi.Environment['MUNARIUM_HTTP_ADDR'] = "127.0.0.1:$($ports[0])"
    $psi.Environment['MUNARIUM_OPS_ADDR'] = "127.0.0.1:$($ports[1])"
    $psi.Environment['MUNARIUM_GRPC_ADDR'] = if ($Grpc) { "127.0.0.1:$($ports[2])" } else { 'disabled' }
    $psi.Environment['MUNARIUM_INSTANCE_ID'] = $Name
    $process = [Diagnostics.Process]::Start($psi)
    $script:Validation.Processes.Add($process)
    $resource = [ordered]@{ kind = 'process'; id = $process.Id; name = $Name; executable = $psi.FileName; started_at = $process.StartTime.ToUniversalTime().ToString('o'); exit_code = $null; cleanup = 'pending' }
    $script:Validation.Receipt.resources += $resource
    $script:Validation.Receipt.tools[$psi.FileName] = @{ sha256 = (Get-FileHash -LiteralPath $psi.FileName).Hash }
    Save-ValidationReceipt
    foreach ($i in 1..60) {
        if ($process.HasExited) { $resource.exit_code = $process.ExitCode; throw 'server_exited' }
        try {
            $null = Invoke-WebRequest "$http/healthz" -TimeoutSec 1
            if ($Grpc) {
                $tcp = [Net.Sockets.TcpClient]::new()
                try { $tcp.Connect('127.0.0.1', $ports[2]) } finally { $tcp.Dispose() }
            }
            if ($process.HasExited) { $resource.exit_code = $process.ExitCode; throw 'server_exited' }
            return @{ Http = $http; Grpc = "http://127.0.0.1:$($ports[2])" }
        } catch { Start-Sleep -Seconds 1 }
    }
    throw 'readiness_timeout'
}

function Invoke-ValidationLiveTier {
    param([string]$Tier, [switch]$Postgres)
    $tenant = $script:Validation.Receipt.run_id + '-' + $Tier
    $values = @{
        MUNARIUM_STORE = 'memory'; MUNARIUM_SOURCE_STORE = 'mem'; MUNARIUM_AUTH_MODE = 'static'
        MUNARIUM_STATIC_TOKENS = "test-rw:${tenant}:rw,test-mgmt:${tenant}:mgmt"
        MUNARIUM_TOKEN_SECRET = 'validation-only-token-secret-32-bytes'
    }
    if ($Postgres) {
        if (-not $script:Validation.DatabaseUrl) { throw 'missing_database' }
        $values.MUNARIUM_STORE = 'postgres'; $values.MUNARIUM_SOURCE_STORE = 'pg'
        $values.MUNARIUM_DATABASE_URL = $script:Validation.DatabaseUrl
    }
    if ($Tier -eq 'cluster') { $values.MUNARIUM_REGISTRY_TTL_SECS = '1'; $values.MUNARIUM_REPLICA_COUNT = '2' }
    $a = Start-ValidationServer "$Tier-a" $values -Grpc:($Tier -eq 'blackbox')
    $suffix = if ($IsWindows) { '.exe' } else { '' }
    $exe = Join-Path $script:Validation.Target "debug/mmp-conformance$suffix"
    switch ($Tier) {
        blackbox { Invoke-ValidationCommand $exe @('--http',$a.Http,'--grpc',$a.Grpc,'--token','test-rw') }
        platform { Invoke-ValidationCommand $exe @('--platform',$a.Http,'--rw-token','test-rw','--mgmt-token','test-mgmt') }
        cluster {
            $b = Start-ValidationServer 'cluster-b' $values
            Invoke-ValidationCommand $exe @('--cluster',$a.Http,'--peer',$b.Http,'--token','test-rw')
        }
    }
}

function Add-ValidationTiers {
    param([bool]$Postgres, [bool]$BlackBox, [bool]$Platform, [bool]$Cluster, [bool]$Gate = $false)
    if ($Postgres -or $Platform -or $Cluster) { Add-ValidationStep 'postgres.setup' { Start-ValidationPostgres } -Requires docker }
    $deps = if ($Postgres) { @('postgres.setup') } else { @() }
    $body = if ($Postgres) {
        { Invoke-ValidationEnvironment @{ MUNARIUM_TEST_DATABASE_URL = $script:Validation.DatabaseUrl } { Invoke-CatalogCommand 'tests.workspace' -Tests } }
    } else {
        { Invoke-ValidationEnvironment @{ MUNARIUM_TEST_DATABASE_URL = $null } { Invoke-CatalogCommand 'tests.workspace' -Tests } }
    }
    Add-ValidationStep 'tests.workspace' $body -Requires cargo -DependsOn $deps
    Add-CatalogSteps @('conformance.memory')
    if ($Gate) {
        Add-ValidationStep 'tests.diskann' {
            Invoke-ValidationEnvironment @{ MUNARIUM_TEST_DATABASE_URL = $script:Validation.DatabaseUrl } { Invoke-CatalogCommand 'tests.diskann' -Tests }
        } -Requires cargo -DependsOn 'postgres.setup'
    }
    if ($Postgres) {
        Add-ValidationStep 'tests.json-postgres' {
            Invoke-ValidationEnvironment @{ MUNARIUM_TEST_DATABASE_URL = $script:Validation.DatabaseUrl } {
                Invoke-ValidationCargoTests @('test','-p','munarium-server','json_feature_pg_','--','--ignored')
            }
        } -Requires cargo -DependsOn 'postgres.setup'
        Add-ValidationStep 'conformance.postgres' { Invoke-ValidationCommand cargo @('run','-p','mmp-conformance','--','--postgres',$script:Validation.DatabaseUrl) } -Requires cargo -DependsOn 'postgres.setup'
    }
    if ($BlackBox -or $Platform -or $Cluster) {
        Add-ValidationStep 'server.build' {
            $metadata = (Invoke-ValidationCommand cargo @('metadata','--format-version','1','--no-deps') -Capture) -join "`n" | ConvertFrom-Json
            if (-not $metadata.target_directory) { throw 'malformed_output' }
            $script:Validation.Target = $metadata.target_directory
            Invoke-ValidationCommand cargo @('build','-p','munarium-server','-p','mmp-conformance')
        } -Requires cargo
    }
    if ($BlackBox) {
        $deps = @('server.build'); if ($Gate) { $deps += 'postgres.setup' }
        $body = if ($Gate) { { Invoke-ValidationLiveTier blackbox -Postgres } } else { { Invoke-ValidationLiveTier blackbox } }
        Add-ValidationStep 'conformance.blackbox' $body -DependsOn $deps
    }
    if ($Platform) { Add-ValidationStep 'conformance.platform' { Invoke-ValidationLiveTier platform -Postgres } -DependsOn @('server.build','postgres.setup') }
    if ($Cluster) { Add-ValidationStep 'conformance.cluster' { Invoke-ValidationLiveTier cluster -Postgres } -DependsOn @('server.build','postgres.setup') }
}
