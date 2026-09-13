param(
    [Parameter(Mandatory = $true)][string]$Binary,
    [Parameter(Mandatory = $true)][string]$Platform,
    [Parameter(Mandatory = $true)][string]$Evidence,
    [ValidateSet("fake", "real-openrouter")][string]$Mode = "fake"
)

$ErrorActionPreference = "Stop"
if (-not $Platform.StartsWith("windows-")) {
    throw "unsupported Windows onboarding platform"
}
if (-not [System.IO.Path]::IsPathFullyQualified($Binary)) {
    throw "installed binary path must be absolute"
}
$binaryItem = Get-Item -LiteralPath $Binary -ErrorAction Stop
if ($binaryItem.PSIsContainer -or (($binaryItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0)) {
    throw "installed binary is unavailable"
}
$installed = $binaryItem.FullName

$smokeRoot = Join-Path $env:RUNNER_TEMP ("heycode-onboarding-" + [guid]::NewGuid().ToString("N"))
$homeRoot = Join-Path $smokeRoot "home"
$workspaceRoot = Join-Path $smokeRoot "workspace"
New-Item -ItemType Directory -Force -Path $homeRoot, $workspaceRoot | Out-Null

try {
    Push-Location $workspaceRoot
    $env:HEYCODE_HOME = $homeRoot
    if ($Mode -eq "fake") {
        $output = & $installed --restricted-workspace --fake run "fresh-machine deterministic release smoke"
        if ($LASTEXITCODE -ne 0 -or ($output -join "`n") -notmatch "FAKE-REPLY") {
            throw "fresh-machine turn did not settle through the fake provider"
        }
        $turn = "deterministic_fake"
    }
    else {
        if ([string]::IsNullOrWhiteSpace($env:OPENROUTER_API_KEY)) {
            throw "real-provider onboarding credential is unavailable"
        }
        $output = & $installed --restricted-workspace `
            --provider openrouter --model z-ai/glm-5.3-flash `
            --set llm.api_key_env=OPENROUTER_API_KEY run `
            "Reply with exactly Q16_REAL_PROVIDER_OK. Do not call tools."
        if ($LASTEXITCODE -ne 0 -or ($output -join "`n") -notmatch "Q16_REAL_PROVIDER_OK") {
            throw "fresh-machine real-provider turn did not settle"
        }
        $turn = "real_provider"
    }
    $runId = if ($env:GITHUB_RUN_ID) { [uint64]$env:GITHUB_RUN_ID } else { 0 }
    $record = [ordered]@{
        schema_version = 1
        platform = $Platform
        source = [ordered]@{ kind = "hosted_native"; run_id = $runId }
        checks = @("attestation_verified", "fresh_install", "first_run_ready")
        turn = $turn
    }
    $record | ConvertTo-Json -Compress -Depth 4 | Set-Content -LiteralPath $Evidence -Encoding utf8NoBOM
}
finally {
    Pop-Location
    Remove-Item -LiteralPath $smokeRoot -Recurse -Force -ErrorAction SilentlyContinue
}
