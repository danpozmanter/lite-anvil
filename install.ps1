# Build and install lite-anvil for Windows.
# Delegates building to scripts/build-local-win.ps1.
# Usage: .\install.ps1
# Installs to %LOCALAPPDATA%\LiteAnvil and adds it to the user PATH.
#Requires -Version 5.1
$ErrorActionPreference = 'Stop'

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$BuildScript = Join-Path $ScriptDir 'scripts\build-local-win.ps1'
$CargoToml = Join-Path $ScriptDir 'Cargo.toml'

& $BuildScript
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$Version = ''
if (Test-Path $CargoToml) {
    $inPackage = $false
    foreach ($line in Get-Content $CargoToml) {
        if ($line -match '^\[package\]') { $inPackage = $true; continue }
        if ($line -match '^\[') { $inPackage = $false }
        if ($inPackage -and $line -match '^version = "([^"]+)"$') {
            $Version = $Matches[1]
            break
        }
    }
}
if (-not $Version) {
    Write-Error "Could not read version from Cargo.toml"
    exit 1
}

$StageDir = Join-Path $ScriptDir "dist\lite-anvil-$Version-windows-x86_64"
$StagedBinary = Join-Path $StageDir 'lite-anvil.exe'
$StagedData = Join-Path $StageDir 'data'

if (-not (Test-Path $StagedBinary)) {
    Write-Error "Binary not found at $StagedBinary"
    exit 1
}
if (-not (Test-Path $StagedData)) {
    Write-Error "Data directory not found at $StagedData"
    exit 1
}

$InstallDir = Join-Path $env:LOCALAPPDATA 'LiteAnvil'
$DataDest = Join-Path $InstallDir 'data'

New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null

Copy-Item -Path $StagedBinary -Destination (Join-Path $InstallDir 'lite-anvil.exe') -Force

# Replace data directory cleanly to remove stale files from previous installs.
if (Test-Path $DataDest) {
    Remove-Item -Recurse -Force $DataDest
}
Copy-Item -Path $StagedData -Destination $DataDest -Recurse

# Add install directory to user PATH if not already present.
$UserPath = [Environment]::GetEnvironmentVariable('PATH', 'User')
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable('PATH', "$UserPath;$InstallDir", 'User')
    Write-Host "Added $InstallDir to user PATH. Restart your terminal to use 'lite-anvil'."
}

Write-Host "Installed Lite Anvil $Version to $InstallDir\lite-anvil.exe"
