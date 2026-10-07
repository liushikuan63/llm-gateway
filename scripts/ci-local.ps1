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
    # 'scripts' is listed so the syntax + fixture-redaction step can be run on
    # its own. Without it, `-Step scripts` is rejected by ValidateSet and the
    # only way to reach that step is a full `-Step all` run.
    [ValidateSet('scripts', 'fmt', 'clippy', 'check', 'test', 'build', 'manual', 'release', 'plan', 'redaction', 'all')]
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


# scripts/*.cjs are NOT compiled by "npm run build" (only src/ is) and
# verify:manual only checks docs. Measured 2026-10-05: a broken object
# literal in ui-smoke.cjs was committed with CI fully green, because
# nothing ever parsed it. `node --check` costs ~0.1s and closes that gap.
function Invoke-ScriptSyntaxStep {
    # .cjs AND .mjs: the generator scripts added for the protocol contracts are
    # .mjs, and a syntax error there would otherwise only surface when someone
    # happens to run them by hand.
    $scripts = Get-ChildItem (Join-Path $RepoRoot 'scripts') -File |
        Where-Object { $_.Extension -in '.cjs', '.mjs', '.js' }
    Invoke-Step 'scripts' {
        foreach ($s in $scripts) { & node --check $s.FullName }
        # Fixture redaction scan. Kept inside this step rather than getting its
        # own: it only walks tests/fixtures (milliseconds), and a separate step
        # would make `-Step scripts` behave differently from `-Step all`.
        & node (Join-Path $RepoRoot 'scripts/check-fixture-redaction.mjs')
    }
}

function Invoke-FrontendSteps {
    foreach ($n in @('build', 'manual', 'release', 'plan', 'redaction')) {
        switch ($n) {
            'build'   { Invoke-Step $n { & npm run build } }
            'manual'  { Invoke-Step $n { & npm run verify:manual } }
            'release' { Invoke-Step $n { & npm run verify:release } }
            'plan'    { Invoke-Step $n { & npm run verify:plan } }
            # B7 判据 1/2：日志里不得出现凭据标识符。
            # 脚本自带样本自测（判据 2 的「故意泄漏必须红」），
            # 所以这一步同时验扫描器本身还在工作。
            'redaction' { Invoke-Step $n { & npm run verify:redaction } }
        }
    }
}

Set-Location -LiteralPath $RepoRoot
try {
    $rustSteps = @('fmt', 'clippy', 'check', 'test')
    switch ($Step) {
        'all'     { Import-RustEnv; Invoke-ScriptSyntaxStep; Invoke-RustSteps; Invoke-FrontendSteps }
        # Without this case `-Step scripts` falls into the default branch below,
        # whose inner switch has no 'scripts' arm: nothing runs, and reading
        # $LASTEXITCODE then trips StrictMode ("检索不到变量 $LASTEXITCODE").
        # Adding the ValidateSet entry without this line makes it a failing step.
        'scripts' { Invoke-ScriptSyntaxStep }
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
                        'redaction' { & npm run verify:redaction }
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
