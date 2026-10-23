#!/usr/bin/env pwsh
# Install the freshly built installer silently, run the binaries, then uninstall. A smoke test for the installer.
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$root = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$setup = Get-ChildItem (Join-Path $root 'dist') -Filter 'gantry-*-x86_64-windows-setup.exe' | Select-Object -First 1
if (-not $setup) { throw 'no installer found in dist/' }

$dir = Join-Path $env:RUNNER_TEMP 'GantryInstallTest'
if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }

# /TASKS= selects no optional task, so the smoke test touches neither PATH nor the desktop.
Start-Process -Wait -FilePath $setup.FullName -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/TASKS=', "/DIR=$dir"

& "$dir\gantry.exe" --version
if ($LASTEXITCODE -ne 0) { throw 'gantry.exe --version failed' }
& "$dir\gantryd.exe" --version
if ($LASTEXITCODE -ne 0) { throw 'gantryd.exe --version failed' }

$unins = Join-Path $dir 'unins000.exe'
if (Test-Path $unins) { Start-Process -Wait -FilePath $unins -ArgumentList '/VERYSILENT' }
Write-Output 'installer verified'
