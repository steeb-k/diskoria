#Requires -Version 5.1
<#
.SYNOPSIS
    Sets the [package] version in jf-storage-tester\Cargo.toml (interactive prompt).

.NOTES
    Only the version under [package] is changed, not dependency version pins.
    Pair with build-release.ps1 after bumping.
#>
$ErrorActionPreference = 'Stop'

$repoRoot = $PSScriptRoot
$cargoDir = Join-Path $repoRoot 'jf-storage-tester'
$cargoToml = Join-Path $cargoDir 'Cargo.toml'

if (-not (Test-Path $cargoToml)) {
    Write-Error "Cargo.toml not found at $cargoToml - run this script from the JF-Storage-Tester repo root."
}

function Get-JfStorageTesterVersionFromMetadata {
    Push-Location $cargoDir
    try {
        $metaJson = cargo metadata --no-deps --format-version 1 2>&1
        if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed: $metaJson" }
        $meta = $metaJson | ConvertFrom-Json
        $pkg = $meta.packages | Where-Object { $_.name -eq 'jf-storage-tester' } | Select-Object -First 1
        if (-not $pkg) { throw "Could not find package 'jf-storage-tester' in cargo metadata." }
        return $pkg.version
    } finally {
        Pop-Location
    }
}

$current = Get-JfStorageTesterVersionFromMetadata
Write-Host "Current package version: $current" -ForegroundColor Cyan
$newVersion = Read-Host "Enter new version"

if ([string]::IsNullOrWhiteSpace($newVersion)) {
    Write-Error "No version entered."
}

$newVersion = $newVersion.Trim()

if ($newVersion -eq $current) {
    Write-Host "Version unchanged; nothing to do." -ForegroundColor Yellow
    exit 0
}

if ($newVersion -match '["\r\n]') {
    Write-Error 'Version must not contain double quotes or line breaks.'
}

$lines = Get-Content -LiteralPath $cargoToml
$inPackage = $false
$updated = $false
$out = foreach ($line in $lines) {
    if ($line -match '^\s*\[package\]\s*$') {
        $inPackage = $true
        $line
        continue
    }
    if ($line -match '^\s*\[') {
        $inPackage = $false
        $line
        continue
    }
    if ($inPackage -and ($line -match '^(\s*)version\s*=\s*"[^"]*"(\s*(#.*)?)$')) {
        $updated = $true
        "$($matches[1])version = `"$newVersion`"$($matches[2])"
    } else {
        $line
    }
}

if (-not $updated) {
    Write-Error "Could not find package version line in [package] section of $cargoToml"
}

$utf8NoBom = New-Object System.Text.UTF8Encoding $false
[System.IO.File]::WriteAllLines($cargoToml, @($out), $utf8NoBom)

# Verify manifest parses and version matches (cargo rejects invalid version strings)
$verify = Get-JfStorageTesterVersionFromMetadata
if ($verify -ne $newVersion) {
    Write-Error "Version in Cargo.toml does not match expected value after update (got '$verify')."
}

Write-Host "Updated package version to $newVersion" -ForegroundColor Green
