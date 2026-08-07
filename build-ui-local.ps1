# Local Windows UI build helper (machine-specific — repo lives on an exFAT drive).
#
# Why this exists: the repo is on E: (exFAT), which cannot create the symlinks
# pnpm needs (both inside node_modules AND to register the project in its store).
# So the SvelteKit frontend is installed/built on C: (NTFS), and only the built
# `build/` output is copied back to E:. The Rust/Tauri build then runs on E:
# (cargo needs no symlinks).
#
# Usage:
#   .\build-ui-local.ps1              # frontend (on C:) + cargo build -p skattr-ui
#   .\build-ui-local.ps1 -RustOnly    # skip frontend; just cargo build (Rust-only edits)
#   .\build-ui-local.ps1 -Run         # after building, launch the app
#   .\build-ui-local.ps1 -Release     # release profile
param(
    [switch]$RustOnly,
    [switch]$Run,
    [switch]$Release
)

$ErrorActionPreference = "Stop"
$repo = "E:\Myggiz\Development\Claude\skattr"
$src  = "$repo\crates\ui\src-svelte"
$work = "C:\Users\rober\skattr-fe"      # NTFS scratch for pnpm
$dist = "$src\build"
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

if (-not $RustOnly) {
    Write-Host "==> Sync frontend source E: -> C: (NTFS)" -ForegroundColor Cyan
    New-Item -ItemType Directory -Force -Path $work | Out-Null
    robocopy $src $work /MIR /XD node_modules build .svelte-kit /NFL /NDL /NJH /NJS /NP | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "robocopy (source) failed: $LASTEXITCODE" }

    Push-Location $work
    try {
        # --no-frozen-lockfile: the committed pnpm-lock.yaml's esrap patch hash
        # doesn't match what current pnpm 10.34.4 computes (patch-hash algo drift
        # since the lockfile was regenerated), so --frozen-lockfile aborts with
        # ERR_PNPM_LOCKFILE_CONFIG_MISMATCH. Reconcile locally instead; the E:
        # lockfile is untouched (this runs on the C: scratch copy).
        Write-Host "==> pnpm install (on C:)" -ForegroundColor Cyan
        $env:CI = "true"
        corepack pnpm@10 install --no-frozen-lockfile
        if ($LASTEXITCODE -ne 0) { throw "pnpm install failed" }

        Write-Host "==> pnpm build (vite)" -ForegroundColor Cyan
        corepack pnpm@10 build
        if ($LASTEXITCODE -ne 0) { throw "pnpm build failed" }
    } finally { Pop-Location }

    Write-Host "==> Copy build/ back to E:" -ForegroundColor Cyan
    robocopy "$work\build" $dist /MIR /NFL /NDL /NJH /NJS /NP | Out-Null
    if ($LASTEXITCODE -ge 8) { throw "robocopy (build back) failed: $LASTEXITCODE" }
}

if (-not (Test-Path "$dist\index.html")) {
    throw "frontendDist missing ($dist). Run without -RustOnly first."
}

$profileArg = @(); if ($Release) { $profileArg = @("--release") }

# Build the CLI (skattr-cli -> target\...\skattr.exe) FIRST. It's pure Rust —
# no frontend/webview/custom-protocol — and shares skattr-core, so once the GUI
# deps are compiled this is fast. Building it before the GUI means -Run (which
# blocks on the launched GUI) still leaves the CLI ready for headless testing.
Write-Host "==> cargo build -p skattr-cli $profileArg" -ForegroundColor Cyan
Push-Location $repo
try {
    cargo build -p skattr-cli @profileArg
    if ($LASTEXITCODE -ne 0) { throw "cargo build (skattr-cli) failed" }
} finally { Pop-Location }

# custom-protocol makes Tauri load the embedded frontendDist instead of the dev
# server (devUrl). `cargo tauri build` injects it automatically; a raw `cargo build`
# does NOT, so without this a release .exe navigates the webview to
# http://localhost:1420 and shows "localhost refused to connect".
$featureArg = @("--features", "tauri/custom-protocol")
$verb = if ($Run) { "run" } else { "build" }
Write-Host "==> cargo $verb -p skattr-ui $profileArg $featureArg" -ForegroundColor Cyan
Push-Location $repo
try {
    cargo $verb -p skattr-ui @profileArg @featureArg
    if ($LASTEXITCODE -ne 0) { throw "cargo $verb failed" }
} finally { Pop-Location }

Write-Host "==> Done. GUI: skattr-ui.exe  CLI: skattr.exe" -ForegroundColor Green
