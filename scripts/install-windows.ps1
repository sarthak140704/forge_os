# Forge OS — Windows install script.
#
# Bootstraps a clean Windows machine to build and run Forge OS end-to-end:
#   - Rust toolchain (rustup + stable + rustfmt + clippy)
#   - Node.js LTS
#   - Visual Studio Build Tools + Windows SDK (needed for Tauri v2 on Windows)
#   - WebView2 runtime (needed by Tauri v2 webview)
#   - Python 3 (used by the scripts/*.py inspectors)
#   - Frontend npm deps
#   - Cargo fetch (populates the crate cache)
#
# Idempotent: skips anything already installed. Never touches system state
# without an explicit `winget install --accept-*` on that specific package.
#
# Usage (from an ELEVATED PowerShell — needed for winget):
#     powershell -ExecutionPolicy Bypass -File .\scripts\install-windows.ps1
#
# Non-fatal flags:
#     -SkipBuildTools    Assume VS Build Tools is already installed
#     -SkipNode          Assume Node.js is already installed
#     -SkipPython        Assume Python 3 is already installed
#     -Verify            Run cargo check + npm build after install (slow)

[CmdletBinding()]
param(
    [switch]$SkipBuildTools,
    [switch]$SkipNode,
    [switch]$SkipPython,
    [switch]$Verify
)

$ErrorActionPreference = 'Stop'

function Write-Step($msg) {
    Write-Host ""
    Write-Host "==> $msg" -ForegroundColor Cyan
}

function Test-Command($name) {
    return [bool](Get-Command $name -ErrorAction SilentlyContinue)
}

function Install-WithWinget($id, $name) {
    if (-not (Test-Command winget)) {
        Write-Host "  winget not found — install App Installer from the Microsoft Store, or install $name manually." -ForegroundColor Yellow
        return $false
    }
    Write-Host "  installing $name via winget…"
    winget install --id $id --accept-source-agreements --accept-package-agreements --silent
    return $LASTEXITCODE -eq 0
}

# ---------------------------------------------------------------------------

Write-Step "Forge OS — Windows install"
Write-Host "This script installs Rust, Node, VS Build Tools, WebView2, Python."
Write-Host "It is idempotent — safe to re-run."

# ---- 1. Rust toolchain -----------------------------------------------------
Write-Step "Rust toolchain"
if (Test-Command rustup) {
    Write-Host "  rustup present: $(rustup --version)"
} else {
    Write-Host "  installing rustup (default host + stable toolchain)…"
    $rustupInit = Join-Path $env:TEMP "rustup-init.exe"
    Invoke-WebRequest -Uri "https://win.rustup.rs/x86_64" -OutFile $rustupInit
    & $rustupInit -y --default-toolchain stable --profile default
    $env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"
    Remove-Item $rustupInit -Force
}
if (Test-Command rustup) {
    rustup update stable
    rustup component add rustfmt clippy 2>$null
}
if (-not (Test-Command cargo)) {
    Write-Host "  ERROR: cargo still not on PATH. Open a new PowerShell." -ForegroundColor Red
    exit 1
}
Write-Host "  cargo: $(cargo --version)"

# ---- 2. Visual Studio Build Tools -----------------------------------------
Write-Step "Visual Studio Build Tools (C++ toolchain + Windows SDK)"
if ($SkipBuildTools) {
    Write-Host "  -SkipBuildTools set — skipping."
} elseif (Test-Path "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools") {
    Write-Host "  Build Tools 2022 already installed."
} elseif (Test-Path "${env:ProgramFiles}\Microsoft Visual Studio\2022") {
    Write-Host "  Visual Studio 2022 detected — Build Tools not needed."
} else {
    Write-Host "  installing VS Build Tools 2022 (this can take 15+ minutes)…"
    Install-WithWinget "Microsoft.VisualStudio.2022.BuildTools" "VS Build Tools" | Out-Null
    Write-Host "  ⚠ You may need to launch 'Visual Studio Installer' and add:" -ForegroundColor Yellow
    Write-Host "    - 'Desktop development with C++' workload"
    Write-Host "    - Windows 10/11 SDK (latest)"
}

# ---- 3. WebView2 runtime ---------------------------------------------------
Write-Step "WebView2 Runtime (required by Tauri v2)"
$webview2Reg = "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
if (Test-Path $webview2Reg) {
    $ver = (Get-ItemProperty -Path $webview2Reg -ErrorAction SilentlyContinue).pv
    Write-Host "  WebView2 present (version $ver)."
} else {
    Write-Host "  installing WebView2 Evergreen…"
    Install-WithWinget "Microsoft.EdgeWebView2Runtime" "WebView2" | Out-Null
}

# ---- 4. Node.js LTS --------------------------------------------------------
Write-Step "Node.js LTS"
if ($SkipNode) {
    Write-Host "  -SkipNode set — skipping."
} elseif (Test-Command node) {
    Write-Host "  node: $(node --version)"
} else {
    Install-WithWinget "OpenJS.NodeJS.LTS" "Node.js LTS" | Out-Null
    # winget doesn't update the current session's PATH — reload:
    $env:PATH = [Environment]::GetEnvironmentVariable('Path','Machine') + ';' +
                [Environment]::GetEnvironmentVariable('Path','User')
    if (-not (Test-Command node)) {
        Write-Host "  ⚠ node still not on PATH — open a new PowerShell after install." -ForegroundColor Yellow
    } else {
        Write-Host "  node: $(node --version)"
    }
}

# ---- 5. Python 3 -----------------------------------------------------------
Write-Step "Python 3 (for scripts/*.py inspectors)"
if ($SkipPython) {
    Write-Host "  -SkipPython set — skipping."
} elseif (Test-Command python) {
    Write-Host "  python: $(python --version)"
} else {
    Install-WithWinget "Python.Python.3.12" "Python 3.12" | Out-Null
}

# ---- 6. Frontend deps ------------------------------------------------------
Write-Step "Frontend npm install"
$frontend = Join-Path $PSScriptRoot "..\apps\forge-desktop\frontend"
if (Test-Path $frontend) {
    Push-Location $frontend
    try {
        if (Test-Command npm) {
            npm install --no-audit --no-fund
        } else {
            Write-Host "  ⚠ npm not on PATH — skip. Re-run after opening a fresh shell." -ForegroundColor Yellow
        }
    } finally {
        Pop-Location
    }
} else {
    Write-Host "  frontend dir not found at $frontend — skip."
}

# ---- 7. Cargo fetch --------------------------------------------------------
Write-Step "Cargo fetch (populate crate cache)"
Push-Location (Join-Path $PSScriptRoot "..")
try {
    cargo fetch
} finally {
    Pop-Location
}

# ---- 8. Optional: full verify ---------------------------------------------
if ($Verify) {
    Write-Step "Verify: cargo check --workspace"
    Push-Location (Join-Path $PSScriptRoot "..")
    try {
        cargo check --workspace --tests --examples
    } finally {
        Pop-Location
    }
}

Write-Step "Done"
Write-Host "Next steps:"
Write-Host "  1. Open a NEW PowerShell so PATH updates apply."
Write-Host "  2. Set an LLM key, e.g.:"
Write-Host "       `$env:GROQ_API_KEY = '<your-key>'"
Write-Host "  3. Boot the desktop app:"
Write-Host "       cd apps\forge-desktop"
Write-Host "       node .\frontend\node_modules\@tauri-apps\cli\tauri.js dev --config .\src-tauri\tauri.conf.json"
