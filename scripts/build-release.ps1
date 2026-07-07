#Requires -Version 5.1
<#
.SYNOPSIS
    Release build: produces releases\<version>\diskoria.exe (version from Cargo.toml).

.NOTES
    Versioning: bump the `version` field in diskoria\Cargo.toml (or use set-version.ps1).
    Git tags: on the diskoria-binaries GitHub repo, tag releases as `v1.0.5` so GitHub's
    `tag_name` matches what the in-app updater parses.

    Output: releases\<version>\diskoria.exe (release profile, single portable exe).

    Optional: if artifact-signing-metadata.json exists next to this script (in scripts\, or
    ARTIFACT_SIGNING_METADATA points to it), the script can sign diskoria.exe with Azure Artifact Signing.
#>
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path $PSScriptRoot -Parent
$cargoDir = Join-Path $repoRoot 'diskoria'
$releasesDir = Join-Path $repoRoot 'releases'
$exeName = 'diskoria.exe'

if (-not (Test-Path (Join-Path $cargoDir 'Cargo.toml'))) {
    Write-Error "Cargo.toml not found at $cargoDir - run this script from the Diskoria repo root."
}

Push-Location $cargoDir
try {
    $metaJson = cargo metadata --no-deps --format-version 1 2>&1
    if ($LASTEXITCODE -ne 0) { throw "cargo metadata failed: $metaJson" }
    $meta = $metaJson | ConvertFrom-Json
    $pkg = $meta.packages | Where-Object { $_.name -eq 'diskoria' } | Select-Object -First 1
    if (-not $pkg) { throw "Could not find package 'diskoria' in cargo metadata." }
    $version = $pkg.version
} finally {
    Pop-Location
}

if ($version -notmatch '^\d+\.\d+\.\d+') {
    Write-Warning "Version '$version' is not plain SemVer MAJOR.MINOR.PATCH; output folder will still use it as-is."
}

Write-Host "Building diskoria $version (release)..." -ForegroundColor Cyan
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

# ── Sign with Azure Artifact Signing (optional) ─────────────────────────────
$MetadataPath = if ($env:ARTIFACT_SIGNING_METADATA) { $env:ARTIFACT_SIGNING_METADATA } else { Join-Path $PSScriptRoot 'artifact-signing-metadata.json' }
$ExeToSign = $destExe

$SkipSigning = $false
if (Test-Path $MetadataPath) {
    $answer = Read-Host "Sign $exeName with Azure Artifact Signing? [Y/n]"
    if ($answer -match '^[Nn]') {
        $SkipSigning = $true
        Write-Host "Signing skipped by user." -ForegroundColor Yellow
    }
}

if (-not $SkipSigning -and (Test-Path $MetadataPath)) {
    $SignToolExe = $env:SIGNTOOL_PATH
    if (-not $SignToolExe -or -not (Test-Path $SignToolExe)) {
        $KitsBin = "C:\Program Files (x86)\Windows Kits\10\bin"
        if (Test-Path $KitsBin) {
            $Latest = Get-ChildItem $KitsBin -Directory | Where-Object { $_.Name -match "^\d+\.\d+\.\d+" } | Sort-Object { [version]($_.Name -replace "^(\d+\.\d+\.\d+).*", '$1') } -Descending | Select-Object -First 1
            if ($Latest) {
                $Candidate = Join-Path (Join-Path $KitsBin $Latest.Name) "x64\signtool.exe"
                if (Test-Path $Candidate) { $SignToolExe = $Candidate }
            }
        }
    }
    if (-not $SignToolExe) {
        $cmd = Get-Command signtool.exe -ErrorAction SilentlyContinue
        if ($cmd) { $SignToolExe = $cmd.Source }
    }

    $DlibPath = $env:ARTIFACT_SIGNING_DLIB
    if (-not $DlibPath -or -not (Test-Path $DlibPath)) {
        $SearchRoots = @(
            "$env:LOCALAPPDATA\Microsoft\MicrosoftArtifactSigningClientTools",
            "$env:LOCALAPPDATA\Microsoft\ArtifactSigningTools",
            "$env:LOCALAPPDATA\Microsoft\TrustedSigningClientTools",
            "C:\ProgramData\Microsoft\MicrosoftTrustedSigningClientTools",
            "C:\Program Files\Microsoft\Azure Artifact Signing Client Tools",
            "C:\Program Files (x86)\Microsoft\Azure Artifact Signing Client Tools",
            "C:\Program Files (x86)\Windows Kits\AzureCodeSigning"
        )
        foreach ($root in $SearchRoots) {
            if (Test-Path $root) {
                $found = Get-ChildItem -Path $root -Recurse -Filter "Azure.CodeSigning.Dlib.dll" -ErrorAction SilentlyContinue | Select-Object -First 1
                if ($found) { $DlibPath = $found.FullName; break }
            }
        }
    }

    if ($SignToolExe -and (Test-Path $SignToolExe) -and $DlibPath -and (Test-Path $DlibPath)) {
        Write-Host ""
        Write-Host "=== Signing with Azure Artifact Signing ===" -ForegroundColor Cyan
        $SignToolArgs = @(
            "sign", "/v", "/fd", "SHA256",
            "/tr", "http://timestamp.acs.microsoft.com", "/td", "SHA256",
            "/dlib", $DlibPath, "/dmdf", $MetadataPath,
            $ExeToSign
        )
        & $SignToolExe $SignToolArgs
        if ($LASTEXITCODE -ne 0) {
            Write-Warning "Signing failed (exit code $LASTEXITCODE). Output exe is still present but unsigned."
        } else {
            Write-Host "  Signed $exeName" -ForegroundColor Green
        }
    } else {
        Write-Host ""
        Write-Host "Signing skipped: SignTool or Artifact Signing dlib not found. Install Artifact Signing Client Tools and Windows SDK." -ForegroundColor Yellow
    }
} elseif (-not (Test-Path $MetadataPath)) {
    Write-Host ""
    Write-Host "Signing skipped: no artifact-signing-metadata.json (optional)." -ForegroundColor Gray
}

Write-Host "OK: $destExe" -ForegroundColor Green
