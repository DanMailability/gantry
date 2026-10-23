#!/usr/bin/env pwsh
# Build dist\gantry-<version>-x86_64-windows-setup.exe (run on Windows, by hand or by .github/workflows/release.yml).
# Needs Rust (rust-toolchain.toml) and Inno Setup 6 (ISCC.exe).
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$match = Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version = "(.*)"$' | Select-Object -First 1
if (-not $match) { throw 'could not read version from Cargo.toml' }
$version = $match.Matches[0].Groups[1].Value

Push-Location $root
try {
    # gantry.exe plus gantryd.exe (Gantry without its window), both bins of the gantry crate.
    cargo build --release -p gantry
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

    $stage = Join-Path $root "dist\gantry-$version"
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    New-Item -ItemType Directory -Force -Path $stage | Out-Null

    Copy-Item 'target\release\gantry.exe'  $stage
    Copy-Item 'target\release\gantryd.exe' $stage
    Copy-Item 'LICENSE-MIT', 'LICENSE-APACHE', 'NOTICE', 'THIRD_PARTY_LICENSES.html' $stage
    # The embedded interface font's licence.
    Copy-Item 'crates\ui-gpui\assets\fonts\OFL.txt' (Join-Path $stage 'LICENSE-FONT-FIGTREE')

    $iscc = @(
        (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe"
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if (-not $iscc) { throw 'Inno Setup 6 (ISCC.exe) not found. Install it: choco install innosetup  (or https://jrsoftware.org/isdl.php)' }

    $outName = "gantry-$version-x86_64-windows-setup"
    & $iscc "/DAppVersion=$version" "/DStageDir=$stage" "/DOutputDir=$root\dist" "/DOutputBaseFilename=$outName" "$root\packaging\windows\gantry.iss"
    if ($LASTEXITCODE -ne 0) { throw 'ISCC failed' }

    $out = Join-Path $root "dist\$outName.exe"
    if (-not (Test-Path $out)) { throw "installer not produced: $out" }
    Write-Output $out
}
finally {
    Pop-Location
}
