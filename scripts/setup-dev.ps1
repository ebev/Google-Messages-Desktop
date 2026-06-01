#Requires -Version 5.1
<#
.SYNOPSIS
  Developer environment setup for Google Messages for Desktop (Windows).

.DESCRIPTION
  Installs (via winget): Visual Studio C++ Build Tools, the WebView2 runtime,
  Rust (rustup, MSVC toolchain) and Node.js LTS, then runs `npm install`.
  Safe to re-run — anything already present is skipped.

  Run from an elevated PowerShell so machine-wide installers can complete:
    powershell -ExecutionPolicy Bypass -File .\scripts\setup-dev.ps1
#>
$ErrorActionPreference = 'Stop'

function Info($m) { Write-Host "==> $m" -ForegroundColor Cyan }
function Warn($m) { Write-Host "warn: $m" -ForegroundColor Yellow }

$ProjectDir = Split-Path -Parent $PSScriptRoot

if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
  Warn "winget not found. Install 'App Installer' from the Microsoft Store, then re-run."
  Warn "Or install the prerequisites manually: https://tauri.app/start/prerequisites/"
  exit 1
}

function Test-WingetInstalled($id) {
  winget list --id $id -e --accept-source-agreements *> $null
  return ($LASTEXITCODE -eq 0)
}

function Install-Winget($id, $name, $override) {
  if (Test-WingetInstalled $id) {
    Info "$name already installed"
    return
  }
  Info "Installing $name…"
  if ($override) {
    winget install -e --id $id --accept-source-agreements --accept-package-agreements --override $override
  } else {
    winget install -e --id $id --accept-source-agreements --accept-package-agreements
  }
}

# 1. Visual Studio C++ Build Tools — MSVC linker (link.exe) + Windows SDK that
#    Rust's MSVC toolchain needs.
#
#    NOTE: it's not enough for the Build Tools *package* to exist — a machine can
#    have the installer present without the C++ workload, which leaves link.exe
#    missing. So we detect the actual VC toolset component with vswhere and only
#    then add the workload (modifying an existing install if needed).
function Test-VCTools {
  $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
  if (-not (Test-Path $vswhere)) { return $false }
  $path = & $vswhere -products '*' -latest `
    -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 `
    -property installationPath 2>$null
  return [bool]$path
}

if (Test-VCTools) {
  Info "Visual Studio C++ Build Tools (VC toolset) already installed"
} else {
  Info "Installing Visual Studio C++ Build Tools (Desktop development with C++)…"
  # --force so winget runs the installer even if the base package is already
  # present; --add modifies that install to include the C++ workload.
  winget install -e --id Microsoft.VisualStudio.2022.BuildTools --force `
    --accept-source-agreements --accept-package-agreements `
    --override '--passive --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
}

# 2. WebView2 runtime (usually present on Windows 11; ensure it on Windows 10).
Install-Winget 'Microsoft.EdgeWebView2Runtime' 'WebView2 Runtime'

# 3. Rust + Node
Install-Winget 'Rustlang.Rustup' 'Rust (rustup)'
Install-Winget 'OpenJS.NodeJS.LTS' 'Node.js LTS'

# Refresh PATH for this session so cargo/node/npm are usable immediately.
$env:Path = [System.Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
            [System.Environment]::GetEnvironmentVariable('Path', 'User')

# Make sure Rust uses the MSVC toolchain (matches the Build Tools above).
if (Get-Command rustup -ErrorAction SilentlyContinue) {
  rustup default stable-msvc | Out-Null
}

# 4. JS dependencies
Info "Installing npm dependencies…"
Push-Location $ProjectDir
try {
  npm install
} finally {
  Pop-Location
}

Info "Done."
Info "Start the app with:  npm run dev"
Warn "If cargo/node aren't found in this window, open a new terminal so PATH refreshes."
