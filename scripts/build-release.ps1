# Build a signed OneRust release and write latest.json for the in-app updater.
#
# Prerequisites:
#   - Private key at %USERPROFILE%\.tauri\onerust.key  (never commit this)
#   - Public key already baked into src-tauri/tauri.conf.json
#
# Usage (from repo root):
#   .\scripts\build-release.ps1
#   .\scripts\build-release.ps1 -Upload   # also gh release upload latest.json + .sig
#
# Env overrides:
#   $env:TAURI_SIGNING_PRIVATE_KEY_PATH = "C:\path\to\onerust.key"

param(
    [switch]$Upload,
    [string]$Tag = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

$keyPath = if ($env:TAURI_SIGNING_PRIVATE_KEY_PATH) {
    $env:TAURI_SIGNING_PRIVATE_KEY_PATH
} else {
    Join-Path $env:USERPROFILE ".tauri\onerust.key"
}
if (-not (Test-Path $keyPath)) {
    throw "Signing private key not found at $keyPath. Generate with: npm run tauri signer generate -- -w `$env:USERPROFILE\.tauri\onerust.key --ci"
}

$env:TAURI_SIGNING_PRIVATE_KEY = (Get-Content -Raw $keyPath).Trim()
$env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = ""
# Prefer path if the CLI supports it; content env is the documented requirement.
$env:TAURI_SIGNING_PRIVATE_KEY_PATH = $keyPath

Write-Host "Building with updater signatures (key: $keyPath)…"
npm run tauri build
if ($LASTEXITCODE -ne 0) { throw "tauri build failed" }

$meta = cargo metadata --manifest-path src-tauri/Cargo.toml --format-version 1 --no-deps | ConvertFrom-Json
$targetDir = $meta.target_directory
$version = (Get-Content package.json | ConvertFrom-Json).version
if (-not $Tag) { $Tag = "v$version" }

$nsisExe = Join-Path $targetDir "release\bundle\nsis\OneRust_${version}_x64-setup.exe"
$nsisSig = "$nsisExe.sig"
$msi = Join-Path $targetDir "release\bundle\msi\OneRust_${version}_x64_en-US.msi"
$msiSig = "$msi.sig"

foreach ($p in @($nsisExe, $nsisSig)) {
    if (-not (Test-Path $p)) { throw "Missing build artifact: $p" }
}

$nsisSigText = (Get-Content $nsisSig -Raw).Trim()
$platforms = @{
    "windows-x86_64" = @{
        url       = "https://github.com/jfmiller52/OneRust/releases/download/$Tag/OneRust_${version}_x64-setup.exe"
        signature = $nsisSigText
    }
}

# Include MSI if present (optional second platform entry uses same key — updater
# picks NSIS via bundle type; we publish NSIS as the primary windows-x86_64 url).
if ((Test-Path $msi) -and (Test-Path $msiSig)) {
    Write-Host "MSI also signed: $msi"
}

$latest = [ordered]@{
    version  = $version
    notes    = "OneRust $version"
    pub_date = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    platforms = $platforms
}

$outDir = Join-Path $targetDir "release\bundle"
$latestPath = Join-Path $outDir "latest.json"
($latest | ConvertTo-Json -Depth 6) | Set-Content -Path $latestPath -Encoding utf8
Write-Host "Wrote $latestPath"

Write-Host ""
Write-Host "Artifacts:"
Write-Host "  $nsisExe"
Write-Host "  $nsisSig"
if (Test-Path $msi) { Write-Host "  $msi" }
if (Test-Path $msiSig) { Write-Host "  $msiSig" }
Write-Host "  $latestPath"

if ($Upload) {
    Write-Host ""
    Write-Host "Uploading to GitHub release $Tag…"
    $assets = @($nsisExe, $nsisSig, $latestPath)
    if (Test-Path $msi) { $assets += $msi }
    if (Test-Path $msiSig) { $assets += $msiSig }
    gh release upload $Tag @assets --clobber
    if ($LASTEXITCODE -ne 0) { throw "gh release upload failed" }
    Write-Host "Done. Updater endpoint: https://github.com/jfmiller52/OneRust/releases/latest/download/latest.json"
} else {
    Write-Host ""
    Write-Host "Next: create/publish the GitHub release, then:"
    Write-Host "  gh release upload $Tag `"$nsisExe`" `"$nsisSig`" `"$latestPath`" --clobber"
    if (Test-Path $msi) {
        Write-Host "  gh release upload $Tag `"$msi`" `"$msiSig`" --clobber"
    }
}
