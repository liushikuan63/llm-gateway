#Requires -Version 5.1
<#
.SYNOPSIS
    Local mirror of .github/workflows/ci.yml.

.DESCRIPTION
    Every step here must match ci.yml in the same order and with the same
    arguments. The point is that CI is not a script path nobody has ever run:
    if the gate can go red locally, it can go red on the runner too.

    ASCII ONLY in this file. scripts/cargo-env.ps1 documents why: a BOM-less
    UTF-8 script gets parsed as GBK on Chinese Windows, and CJK comment bytes
    break statement boundaries. Same reason applies here.

    What is deliberately NOT part of the gate (see ci.yml header for the
    measured reasons):
      - tauri:build  (the MSI step downloads WiX and its downloader times out
        on a 39MB file even though a direct download of the same URL is fine)
      - verify:ui    (needs a running dev server; Vite binds IPv6 only)
      - cargo test -- --ignored  (real upstream smoke tests need credentials)

.PARAMETER Step
    Run a single step by name: fmt, clippy, check, test, build, manual,
    release, plan, all (default).
#>
[CmdletBinding()]
param(
    [ValidateSet('fmt', 'clippy', 'check', 'test', 'build', 'manual', 'release', 'plan', 'all')]
    [string]$Step = 'all'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$RepoRoot = Split-Path -Parent $PSScriptRoot
if (-not $RepoRoot) { $RepoRoot = (Get-Location).Path }

# Cargo.toml lives under src-tauri, not at the repo root. Every cargo command
# therefore needs --manifest-path, otherwise it fails with "could not find
# Cargo.toml". ci.yml uses the same path.
$ManifestPath = Join-Path $RepoRoot 'src-tauri\Cargo.toml'

$script:Failed = New-Object System.Collections.Generic.List[string]

function Invoke-Step {
    param(
        [Parameter(Mandatory)][string]$Name,
        [Parameter(Mandatory)][scriptblock]$Action
    )
    Write-Host ""
    Write-Host "=== [$Name] ===" -ForegroundColor Cyan
    $sw = [Diagnostics.Stopwatch]::StartNew()
    try {
        & $Action
        $code = $LASTEXITCODE
        if ($null -ne $code -and $code -ne 0) { throw "exit code $code" }
        Write-Host ("--- {0} OK ({1:N0}s)" -f $Name, $sw.Elapsed.TotalSeconds) -ForegroundColor Green
    }
    catch {
        Write-Host ("--- {0} FAILED after {1:N0}s" -f $Name, $sw.Elapsed.TotalSeconds) -ForegroundColor Red
        Write-Host $_.Exception.Message -ForegroundColor Red
        $script:Failed.Add($Name)
    }
}

# The MSVC toolchain is not on PATH on this machine (the rustup shim is gone).
# cargo-env.ps1 also changes the current directory to src-tauri, so every path
# used afterwards must be absolute or repo-rooted.
function Import-RustEnv {
    Write-Host "=== [env] ===" -ForegroundColor Cyan
    $cargoEnv = Join-Path $RepoRoot 'scripts\cargo-env.ps1'
    if (-not (Test-Path -LiteralPath $cargoEnv)) { throw "missing $cargoEnv" }
    & $cargoEnv
    Set-Location -LiteralPath $RepoRoot
    Write-Host "--- env ready" -ForegroundColor Green
}

function Invoke-RustSteps {
    foreach ($n in @('fmt', 'clippy', 'check', 'test')) {
        switch ($n) {
            'fmt'    { Invoke-Step $n { & cargo fmt --manifest-path $ManifestPath --all -- --check } }
            'clippy' { Invoke-Step $n { & cargo clippy --manifest-path $ManifestPath --all-targets -- -D warnings } }
            'check'  { Invoke-Step $n { & cargo check --manifest-path $ManifestPath --all-targets --jobs 1 } }
            'test'   { Invoke-Step $n { & cargo test --manifest-path $ManifestPath --jobs 1 } }
        }
    }
}

function Invoke-FrontendSteps {
    foreach ($n in @('build', 'manual', 'release', 'plan')) {
        switch ($n) {
            'build'   { Invoke-Step $n { & npm run build } }
            'manual'  { Invoke-Step $n { & npm run verify:manual } }
            'release' { Invoke-Step $n { & npm run verify:release } }
            'plan'    { Invoke-Step $n { & npm run verify:plan } }
        }
    }
}

Set-Location -LiteralPath $RepoRoot
try {
    $rustSteps = @('fmt', 'clippy', 'check', 'test')
    switch ($Step) {
        'all'     { Import-RustEnv; Invoke-RustSteps; Invoke-FrontendSteps }
        default   {
            if ($rustSteps -contains $Step) {
                Import-RustEnv
                Invoke-Step $Step {
                    switch ($Step) {
                        'fmt'    { & cargo fmt --manifest-path $ManifestPath --all -- --check }
                        'clippy' { & cargo clippy --manifest-path $ManifestPath --all-targets -- -D warnings }
                        'check'  { & cargo check --manifest-path $ManifestPath --all-targets --jobs 1 }
                        'test'   { & cargo test --manifest-path $ManifestPath --jobs 1 }
                    }
                }
            }
            else {
                Invoke-Step $Step {
                    switch ($Step) {
                        'build'   { & npm run build }
                        'manual'  { & npm run verify:manual }
                        'release' { & npm run verify:release }
                        'plan'    { & npm run verify:plan }
                    }
                }
            }
        }
    }
}
finally {
    Set-Location -LiteralPath $RepoRoot
}

Write-Host ""
if ($script:Failed.Count -gt 0) {
    Write-Host ("CI FAILED: " + ($script:Failed -join ', ')) -ForegroundColor Red
    exit 1
}
Write-Host "CI OK" -ForegroundColor Green
exit 0
