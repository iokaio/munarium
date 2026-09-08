# SPDX-License-Identifier: Apache-2.0
#Requires -Version 7
[CmdletBinding()]
param(
    [string]$Endpoint = 'http://127.0.0.1:11434',
    [string]$OutputPath = (Join-Path $PSScriptRoot '../../scratch/ollama/environment.json')
)

$ErrorActionPreference = 'Stop'
$Endpoint = $Endpoint.TrimEnd('/')
function Api([string]$Path, $Body) {
    if ($null -eq $Body) {
        return Invoke-RestMethod "$Endpoint$Path" -TimeoutSec 180
    }
    $json = ConvertTo-Json -InputObject $Body -Depth 12 -Compress
    return Invoke-RestMethod "$Endpoint$Path" -Method Post -ContentType 'application/json' -Body $json -TimeoutSec 180
}

# Declare expected answers before querying. These are small environment checks,
# not a general model benchmark or the later Munarium integration acceptance.
$cases = @(
    @{ name = 'code-cold'; prompt = 'The project code is MAPLE. What is the project code? Reply with only the code.'; expected = 'MAPLE' },
    @{ name = 'code-warm'; prompt = 'The project code is MAPLE. What is the project code? Reply with only the code.'; expected = 'MAPLE' },
    @{ name = 'grounded-extraction'; prompt = 'Facts: The Cedar observatory opens at 09:00. Its curator is Mira Chen. What is the curator name? Reply with only the name.'; expected = 'Mira Chen' },
    @{ name = 'unknown-fact'; prompt = 'Facts: The Cedar observatory opens at 09:00. Its curator is Mira Chen. The facts do not state a closing time. What time does it close? Reply with only UNKNOWN if not stated.'; expected = 'UNKNOWN' }
)
$version = Api '/api/version' $null
$tags = Api '/api/tags' $null
$expectedModels = Get-Content (Join-Path $PSScriptRoot 'models.json') -Raw | ConvertFrom-Json -AsHashtable
foreach ($model in $expectedModels.Keys) {
    $installed = @($tags.models | Where-Object { $_.name -eq $model })
    if ($installed.Count -ne 1) { throw "Required model missing or ambiguous: $model" }
    if ($installed[0].digest -cne $expectedModels[$model]) {
        throw "Model digest changed for $model; review and re-evaluate before updating models.json."
    }
}
# Unload Qwen before measuring cold loading. This affects only this endpoint.
$null = Api '/api/generate' @{ model = 'qwen3:1.7b'; keep_alive = 0; stream = $false }
$results = foreach ($case in $cases) {
    $timer = [Diagnostics.Stopwatch]::StartNew()
    $answer = Api '/api/chat' @{
        model = 'qwen3:1.7b'
        stream = $false
        think = $false
        messages = @(@{ role = 'user'; content = $case.prompt })
        options = @{ temperature = 0; num_predict = 64; num_ctx = 4096; num_thread = 4 }
    }
    $timer.Stop()
    $content = $answer.message.content.Trim()
    $passed = $answer.done -eq $true -and $answer.done_reason -eq 'stop' -and $content -ceq $case.expected
    [pscustomobject][ordered]@{
        name = $case.name; passed = $passed; prompt = $case.prompt; expected = $case.expected; answer = $content
        wallSeconds = [Math]::Round($timer.Elapsed.TotalSeconds, 3)
        loadSeconds = [Math]::Round($answer.load_duration / 1e9, 3)
        inputTokens = $answer.prompt_eval_count; outputTokens = $answer.eval_count
        stopReason = $answer.done_reason
    }
}
$embedding = Api '/api/embed' @{
    model = 'all-minilm:22m'
    input = @('The project code is MAPLE.', 'The meeting starts at noon.')
    truncate = $false
}
$dimensions = $embedding.embeddings[0].Count
$embeddingOk = $embedding.embeddings.Count -eq 2 -and $dimensions -eq 384
foreach ($vector in $embedding.embeddings) {
    if ($vector.Count -ne $dimensions) { $embeddingOk = $false }
    foreach ($value in $vector) {
        if (-not [double]::IsFinite([double]$value)) { $embeddingOk = $false }
    }
}
$passed = @($results | Where-Object { -not $_.passed }).Count -eq 0 -and $embeddingOk
$report = [ordered]@{
    status = $(if ($passed) { 'passed' } else { 'failed' })
    completedUtc = [DateTime]::UtcNow.ToString('o')
    ollamaVersion = $version.version
    models = @($tags.models | Select-Object name, digest, size, details)
    completions = @($results)
    embeddings = @{ passed = $embeddingOk; count = $embedding.embeddings.Count; dimensions = $dimensions; inputTokens = $embedding.prompt_eval_count }
}
$resolvedOutput = [IO.Path]::GetFullPath($OutputPath)
New-Item -ItemType Directory -Force (Split-Path $resolvedOutput) | Out-Null
$report | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $resolvedOutput -Encoding utf8
$results | Format-Table name, passed, answer, wallSeconds, loadSeconds
Write-Host "Embedding vectors: $($embedding.embeddings.Count) x $dimensions; passed: $embeddingOk"
Write-Host "Evidence: $resolvedOutput"
if (-not $passed) { throw 'Ollama environment acceptance failed; inspect the evidence.' }
