# cargo-env.ps1 (MSVC)
# Rust on Windows with Visual Studio Build Tools.
# Run this before any cargo command:  cd D:\Java\GitHub\llm-auto\llm-gateway\src-tauri
#
# Why this file exists:
#   1. Build Tools installed to C:\BuildTools, which is NOT on PATH by default.
#      Without the bin directory, the MSVC linker (link.exe) is not found.
#   2. The Windows SDK libs (kernel32.lib, ucrt.lib, ...) also live outside PATH.
#
# The GNU toolchain path is no longer used. For the record, it could not run
# tauri build scripts on this machine. See docs/install-rust-toolchain.md and
# docs/0.3.0-verification.md (Chinese filenames) for the full write-up.
#
# NOTE ON NON-ASCII (measured 2026-10-06, C4-2):
#   The project rule says "script comments must be pure ASCII" because a
#   UTF-8-without-BOM file is decoded with the ANSI codepage on a Chinese
#   Windows and a mis-decoded comment can shift statement boundaries.
#   That risk is specific to COMMENTS: a comment runs to end-of-line, so a
#   garbled multi-byte sequence can swallow or split the next line.
#   A non-ASCII STRING LITERAL is safe, because the parser tracks quotes and
#   does not depend on the encoding to find the end of the token.
#   Verified by running this file under BOTH pwsh 7 and Windows PowerShell
#   5.1 with no -Encoding flag: exit 0, "cargo env ready" printed, no
#   ParserError. Hence the Chinese path inside the throw below is deliberate.
#   Do NOT put Chinese in a comment here -- that is the case that was never
#   verified and is the one the rule is about.

$ErrorActionPreference = 'Stop'

# Visual Studio Build Tools (C:\BuildTools) and the Windows 10/11 SDK.
$vcRoot    = 'C:\BuildTools\VC\Tools\MSVC'
$vcVersion = (Get-ChildItem $vcRoot -Directory -ErrorAction SilentlyContinue |
              Sort-Object Name -Descending |
              Select-Object -First 1).Name
if (-not $vcVersion) {
    # C4-2: this used to point at scripts/install-buildtools.ps1, which does
    # not exist in this repo -- the message sent people to a dead end at the
    # exact moment they were already stuck. The setup guide that does exist is
    # docs/<chinese name>.md; see the note at the top of this file about why a
    # non-ASCII literal here is deliberate and measured.
    throw "MSVC toolset not found under $vcRoot. Follow docs/安装Rust工具链.md first."
}

$vcBin  = Join-Path $vcRoot "$vcVersion\bin\Hostx64\x64"
$sdkRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10'
$sdkBin  = Join-Path $sdkRoot 'bin'
$sdkLib  = Join-Path $sdkRoot 'Lib'

$paths = @($vcBin)
if (Test-Path $sdkBin) {
    # Prefer the newest SDK version subdirectory, e.g. 10.0.22621.0.
    # Must filter to 10.* explicitly: the SDK bin dir also holds x86/x64/arm64
    # folders, and "x86" sorts AFTER "10.0.x" in a descending string sort.
    $sdkVer = (Get-ChildItem $sdkBin -Directory -ErrorAction SilentlyContinue |
               Where-Object { $_.Name -like '10.*' } |
               Sort-Object Name -Descending |
               Select-Object -First 1).Name
    if ($sdkVer) {
        $paths += (Join-Path $sdkBin "$sdkVer\x64")
        $paths += (Join-Path $sdkLib "$sdkVer\um\x64")
        $paths += (Join-Path $sdkLib "$sdkVer\ucrt\x64")
    }
}

# Prepend. Do not append: link.exe must win over any MinGW ld on PATH.
foreach ($p in ($paths | Where-Object { Test-Path $_ })) {
    $env:PATH = "$p;$env:PATH"
}

# cargo / rustc
#
# Point at the toolchain's own bin, not the rustup shims in ~/.cargo/bin.
# Measured on this machine: after some `rustup toolchain install` runs, that
# shim directory ended up holding ONLY rustup.exe - cargo.exe / rustc.exe /
# rustdoc.exe were gone. Using the toolchain directory sidesteps that and also
# removes one layer of indirection: the cargo we invoke IS the toolchain.
$toolchainBin = Join-Path $env:USERPROFILE `
    '.rustup\toolchains\stable-x86_64-pc-windows-msvc\bin'
if (Test-Path (Join-Path $toolchainBin 'cargo.exe')) {
    $env:PATH = "$toolchainBin;$env:PATH"
} else {
    $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
}
$env:RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-msvc'
$env:CARGO_TERM_COLOR  = 'never'

# No -C link-self-contained here: that flag is GNU-only and the MSVC toolchain
# finds the CRT and kernel32.lib through the SDK paths added above.
$env:CARGO_ENCODED_RUSTFLAGS = ''

Set-Location -LiteralPath (Join-Path $PSScriptRoot '..\src-tauri')
Write-Host "cargo env ready: MSVC $vcVersion, SDK $sdkVer"
