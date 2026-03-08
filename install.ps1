# Install lite-anvil from a local release build.
# Usage: .\install.ps1
# Installs to %LOCALAPPDATA%\LiteAnvil and adds it to the user PATH.
#Requires -Version 5.1
$ErrorActionPreference = 'Stop'

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$Binary    = Join-Path $ScriptDir 'target\release\lite-anvil.exe'
$DataSrc   = Join-Path $ScriptDir 'data'

if (-not (Test-Path $Binary)) {
    Write-Error "Binary not found at $Binary — run 'cargo build --release' first"
    exit 1
}
if (-not (Test-Path $DataSrc)) {
    Write-Error "Data directory not found at $DataSrc"
    exit 1
}

$InstallDir = Join-Path $env:LOCALAPPDATA 'LiteAnvil'
$DataDest   = Join-Path $InstallDir 'data'

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

Copy-Item -Path $Binary -Destination (Join-Path $InstallDir 'lite-anvil.exe') -Force

# Replace data directory cleanly to remove stale files from previous installs.
if (Test-Path $DataDest) {
    Remove-Item -Recurse -Force $DataDest
}
Copy-Item -Path $DataSrc -Destination $DataDest -Recurse

# Add install directory to user PATH if not already present.
$UserPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable('PATH', "$UserPath;$InstallDir", 'User')
    Write-Host "Added $InstallDir to user PATH. Restart your terminal to use 'lite-anvil'."
}

Write-Host "Installed to $InstallDir\lite-anvil.exe"
