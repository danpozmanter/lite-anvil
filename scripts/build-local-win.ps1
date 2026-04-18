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

$env:CMAKE_MSVC_RUNTIME_LIBRARY = 'MultiThreaded'
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

# SDL3 is statically linked via sdl3-sys during `cargo build` — no SDL3.dll
# to ship.
# Bundle vcpkg dynamic dependencies (freetype, pcre2-8, etc.) next to the exe.
# Without these, Windows reports 0xc000007b on systems that don't have them.
$VcpkgBin = 'C:\vcpkg\installed\x64-windows\bin'
if (Test-Path $VcpkgBin) {
    Get-ChildItem -Path $VcpkgBin -Filter *.dll | ForEach-Object {
        Copy-Item -Path $_.FullName -Destination $StageDir
    }
}

# Bundle the Microsoft Visual C++ redistributable runtime (vcruntime140.dll,
# msvcp140.dll, etc.) alongside the exe. Rust is built with +crt-static via
# `.cargo/config.toml` so the editor binaries themselves don't need these, but
# SDL3 / freetype / pcre2 DLLs do, and Windows aborts with
# "VCRUNTIME140.dll was not found" on a clean install otherwise. Microsoft's
# license explicitly allows app-local deployment of the MSVC CRT, so copying
# the entire Microsoft.VC*.CRT folder is legal and forward-compatible.
$VcRedistPatterns = @(
    'C:\Program Files\Microsoft Visual Studio\2022\*\VC\Redist\MSVC\*\x64\Microsoft.VC*.CRT',
    'C:\Program Files (x86)\Microsoft Visual Studio\2022\*\VC\Redist\MSVC\*\x64\Microsoft.VC*.CRT'
)
$VcRedistDir = $null
foreach ($pattern in $VcRedistPatterns) {
    $match = Get-ChildItem -Path $pattern -Directory -ErrorAction SilentlyContinue |
             Sort-Object -Property FullName -Descending |
             Select-Object -First 1
    if ($match) { $VcRedistDir = $match.FullName; break }
}
if ($VcRedistDir) {
    Write-Host "Bundling VC runtime from $VcRedistDir"
    Get-ChildItem -Path "$VcRedistDir\*.dll" | ForEach-Object {
        Copy-Item -Path $_.FullName -Destination $StageDir
    }
} else {
    Write-Warning 'VC++ redistributable directory not found; bundled DLLs may require VCRUNTIME140.dll at runtime.'
}

Compress-Archive -Path $StageDir -DestinationPath $Archive

Write-Host "Built archive: $Archive"
Write-Host "Staging dir:   $StageDir"
