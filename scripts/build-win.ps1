# LLM Gateway Windows 打包脚本
# 用法: powershell -ExecutionPolicy Bypass -File scripts\build-win.ps1
$ErrorActionPreference = "Stop"
Set-Location (Join-Path $PSScriptRoot "..")

function Invoke-Npm {
    param([Parameter(ValueFromRemainingArguments = $true)][string[]]$NpmArguments)

    & npm.cmd @NpmArguments
    if ($LASTEXITCODE -ne 0) {
        throw "npm $($NpmArguments -join ' ') 失败，退出码：$LASTEXITCODE"
    }
}

Write-Host "==> 1/3 安装前端依赖" -ForegroundColor Cyan
if (-not (Test-Path node_modules)) { Invoke-Npm install }

Write-Host "==> 2/3 校验发布配置和图标资源" -ForegroundColor Cyan
Invoke-Npm run verify:release

Write-Host "==> 3/3 构建前端、Rust 和 Windows 安装包" -ForegroundColor Cyan
$buildStartedAt = Get-Date
Invoke-Npm run tauri:build

Write-Host "`n完成。产物位于 src-tauri\target\release\bundle" -ForegroundColor Green
$nsis = Get-ChildItem -Path "src-tauri/target/release/bundle/nsis" -File -Filter *.exe -ErrorAction SilentlyContinue |
    Where-Object { $_.LastWriteTime -ge $buildStartedAt.AddSeconds(-2) }
$msi = Get-ChildItem -Path "src-tauri/target/release/bundle/msi" -File -Filter *.msi -ErrorAction SilentlyContinue |
    Where-Object { $_.LastWriteTime -ge $buildStartedAt.AddSeconds(-2) }

if (-not $nsis -or -not $msi) {
    throw "未找到本次构建生成的 NSIS 和 MSI 安装包，请检查上面的 Tauri 构建输出。"
}

@($nsis) + @($msi) | ForEach-Object {
    $hash = Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256
    $sizeMiB = [Math]::Round($_.Length / 1MB, 2)
    Write-Host "  $($_.FullName) ($sizeMiB MiB, SHA256 $($hash.Hash))"
}
