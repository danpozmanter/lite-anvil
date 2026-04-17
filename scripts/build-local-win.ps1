# Build a local Windows x86_64 release artifact matching the GitHub Actions release output.
# Produces:
#   dist\lite-anvil-${Version}-windows-x86_64\        (staging directory)
#   dist\lite-anvil-${Version}-windows-x86_64.zip     (release archive)
#Requires -Version 5.1
$ErrorActionPreference = 'Stop'

$RootDir = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $RootDir

$CargoToml = Join-Path $RootDir 'Cargo.toml'
$Version = ''
if (Test-Path $CargoToml) {
    $inPackage = $false
    foreach ($line in Get-Content $CargoToml) {
        if ($line -match '^\[workspace\.package\]') { $inPackage = $true; continue }
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

$ArchiveBase = "lite-anvil-$Version-windows-x86_64"
$DistDir = Join-Path $RootDir 'dist'
$StageDir = Join-Path $DistDir $ArchiveBase
$Archive = Join-Path $DistDir "$ArchiveBase.zip"

cargo build --release --workspace
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$Binary = Join-Path $RootDir 'target\release\lite-anvil.exe'
if (-not (Test-Path $Binary)) {
    Write-Error "Binary not found at $Binary"
    exit 1
}

$NanoBinary = Join-Path $RootDir 'target\release\nano-anvil.exe'

if (Test-Path $StageDir) { Remove-Item -Recurse -Force $StageDir }
if (Test-Path $Archive)  { Remove-Item -Force $Archive }
New-Item -ItemType Directory -Force -Path $StageDir | Out-Null

Copy-Item -Path $Binary -Destination $StageDir
if (Test-Path $NanoBinary) {
    Copy-Item -Path $NanoBinary -Destination $StageDir
}
Copy-Item -Path (Join-Path $RootDir 'data') -Destination $StageDir -Recurse
Copy-Item -Path (Join-Path $RootDir 'data-nano') -Destination $StageDir -Recurse
$WindowsResources = Join-Path $RootDir 'resources\windows\*.ps1'
if (Test-Path $WindowsResources) {
    Copy-Item -Path $WindowsResources -Destination $StageDir
}

# Bundle SDL3.dll so it loads from the exe directory at runtime.
$SdlCandidates = @(
    'C:\sdl3-nogl\bin\SDL3.dll',
    (Join-Path $RootDir 'lib\sdl3-nogl\SDL3.dll'),
    (Join-Path $RootDir 'SDL\build\Release\SDL3.dll')
)
$SdlDll = $SdlCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
if ($SdlDll) {
    Copy-Item -Path $SdlDll -Destination $StageDir
} else {
    Write-Warning 'SDL3.dll not found; the package will not run on clean Windows systems.'
}

# Bundle vcpkg dynamic dependencies (freetype, pcre2-8, etc.) next to the exe.
# Without these, Windows reports 0xc000007b on systems that don't have them.
$VcpkgBin = 'C:\vcpkg\installed\x64-windows\bin'
if (Test-Path $VcpkgBin) {
    Get-ChildItem -Path $VcpkgBin -Filter *.dll | ForEach-Object {
        Copy-Item -Path $_.FullName -Destination $StageDir
    }
}

Compress-Archive -Path $StageDir -DestinationPath $Archive

Write-Host "Built archive: $Archive"
Write-Host "Staging dir:   $StageDir"
