#Requires -Version 5.1
<#
.SYNOPSIS
  Build release bundles for Google Messages for Desktop (Windows).

.DESCRIPTION
  Cross-compiles between x64 and ARM64 using the MSVC toolchain — both VC++
  Tools components are installed by scripts\setup-dev.ps1. Defaults to the
  host architecture.

.PARAMETER Target
  'x64' or 'arm64'. Defaults to the host architecture.

.EXAMPLE
  .\scripts\build.ps1
.EXAMPLE
  .\scripts\build.ps1 -Target arm64
#>
param(
  [ValidateSet('x64', 'arm64')]
  [string]$Target
)
$ErrorActionPreference = 'Stop'

function Info($m) { Write-Host "==> $m" -ForegroundColor Cyan }
function Err($m) { Write-Host "error: $m" -ForegroundColor Red }

$ProjectDir = Split-Path -Parent $PSScriptRoot

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  Err "cargo not found. Run scripts\setup-dev.ps1 first."
  exit 1
}

if (-not $Target) {
  $hostTriple = ((& rustc -vV) | Where-Object { $_ -like 'host:*' }) -replace 'host:\s*', ''
  $Target = if ($hostTriple -like 'aarch64*') { 'arm64' } else { 'x64' }
}

$TripleMap = @{ 'x64' = 'x86_64-pc-windows-msvc'; 'arm64' = 'aarch64-pc-windows-msvc' }
$Triple = $TripleMap[$Target]

Info "Building for $Triple"

$installed = (& rustup target list --installed) -join "`n"
if ($installed -notmatch [regex]::Escape($Triple)) {
  Info "Installing rust target $Triple…"
  rustup target add $Triple
}

Push-Location $ProjectDir
try {
  npm run tauri -- build --target $Triple
} finally {
  Pop-Location
}

$BundleDir = Join-Path $ProjectDir "src-tauri\target\$Triple\release\bundle"
Info "Done. Bundles in: $BundleDir"
if (Test-Path $BundleDir) {
  Get-ChildItem -Recurse -File $BundleDir | Select-Object -ExpandProperty FullName
}
