# Build portable Diskoria (release) and copy exe to portable-build/.

$ErrorActionPreference = "Continue"

$ProjectRoot = Split-Path $PSScriptRoot -Parent
$CargoDir    = Join-Path $ProjectRoot "diskoria"
$ReleaseDir  = Join-Path $CargoDir "target\release"
$OutputDir   = Join-Path $ProjectRoot "portable-build"

# Clean output directory
if (Test-Path $OutputDir) {
    try {
        Remove-Item $OutputDir -Recurse -Force -ErrorAction Stop
    } catch {
        # Most common cause: exe in portable-build is currently running/locked.
        $stamp = Get-Date -Format "yyyyMMdd-HHmmss"
        $backup = "$OutputDir.old.$stamp"
        Rename-Item -Path $OutputDir -NewName $backup -ErrorAction SilentlyContinue
    }
}
if (-not (Test-Path $OutputDir)) {
    New-Item -ItemType Directory -Path $OutputDir | Out-Null
}

Write-Host "Building Diskoria (release)..." -ForegroundColor Cyan
Push-Location $CargoDir
try {
    # cargo prints normal progress to stderr; route through cmd.exe to keep output readable.
    & cmd /c "cargo build --release"
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Build failed!" -ForegroundColor Red
        exit 1
    }
} finally {
    Pop-Location
}

Write-Host "Copying executable..." -ForegroundColor Cyan
$ExeName = "diskoria.exe"
$ExePath = Join-Path $ReleaseDir $ExeName
if (-not (Test-Path $ExePath)) {
    $found = Get-ChildItem -Path $ReleaseDir -Filter "*.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($found) {
        $ExePath = $found.FullName
        $ExeName = $found.Name
        Write-Host "Using $ExeName (release folder)" -ForegroundColor Gray
    } else {
        Write-Host "ERROR: Executable not found in $ReleaseDir (expected $ExeName)" -ForegroundColor Red
        exit 1
    }
}
Copy-Item $ExePath -Destination $OutputDir

Write-Host ""
Write-Host "Portable build complete!" -ForegroundColor Green
Write-Host "Output: $OutputDir" -ForegroundColor White
Write-Host ""

Get-ChildItem $OutputDir -Recurse | ForEach-Object {
    if ($_.PSIsContainer) { return }
    $size = "{0:N0} KB" -f ($_.Length / 1KB)
    Write-Host "  $($_.Name) ($size)" -ForegroundColor Gray
}
