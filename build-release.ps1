#Requires -Version 5.1
<#
.SYNOPSIS
    Release build: produces releases\<version>\jf-storage-tester.exe (version from Cargo.toml).

.NOTES
    Versioning (recommended for this app):
    - Use Semantic Versioning: MAJOR.MINOR.PATCH (e.g. 0.2.1).
    - Single source of truth: the `version` field in jf-storage-tester\Cargo.toml under [package].
      Bump it before running this script when you want a new release number (or use set-version.ps1).
    - Git tags: tag releases as `v1.0.5` so GitHub's `tag_name` matches what the in-app updater parses
      (see RUST-CONVERSION.md).

    Output: JF-Storage-Tester\releases\<version>\jf-storage-tester.exe (release profile).
    This script does not sign the binary.
#>
$ErrorActionPreference = 'Stop'

$repoRoot = $PSScriptRoot
$cargoDir = Join-Path $repoRoot 'jf-storage-tester'
$releasesDir = Join-Path $repoRoot 'releases'
$exeName = 'jf-storage-tester.exe'

if (-not (Test-Path (Join-Path $cargoDir 'Cargo.toml'))) {
    Write-Error "Cargo.toml not found at $cargoDir - run this script from the JF-Storage-Tester repo root."
}

Push-Location $cargoDir
try {
    $metaJson = cargo metadata --no-deps --format-version 1 2>&1
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed: $metaJson" }
    $meta = $metaJson | ConvertFrom-Json
    $pkg = $meta.packages | Where-Object { $_.name -eq 'jf-storage-tester' } | Select-Object -First 1
    if (-not $pkg) { throw "Could not find package 'jf-storage-tester' in cargo metadata." }
    $version = $pkg.version
} finally {
    Pop-Location
}

if ($version -notmatch '^\d+\.\d+\.\d+') {
    Write-Warning "Version '$version' is not plain SemVer MAJOR.MINOR.PATCH; output folder name will still use it as-is."
}

Write-Host "Building jf-storage-tester $version (release)..." -ForegroundColor Cyan
Push-Location $cargoDir
try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
} finally {
    Pop-Location
}

$built = Join-Path $cargoDir "target\release\$exeName"
if (-not (Test-Path $built)) {
    Write-Error "Expected binary not found: $built"
}

New-Item -ItemType Directory -Force -Path $releasesDir | Out-Null
$versionDir = Join-Path $releasesDir $version
New-Item -ItemType Directory -Force -Path $versionDir | Out-Null
$destExe = Join-Path $versionDir $exeName
Copy-Item -Path $built -Destination $destExe -Force

Write-Host "OK: $destExe" -ForegroundColor Green
