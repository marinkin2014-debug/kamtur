#Requires -Version 5.1
<#
.SYNOPSIS
    Installs pre-commit git hooks for the kamtur repository.

.DESCRIPTION
    Idempotent. Sequence:

      1. Verify we are inside the kamtur git repository.
      2. Verify cargo is available (without it the hooks are useless).
      3. Locate Python 3.8+ (pre-commit framework is a Python package).
      4. Install pre-commit: pipx (isolated) -> pip --user (fallback).
      5. Validate .pre-commit-config.yaml via `pre-commit validate-config`.
      6. Register pre-commit and pre-push hooks in .git/hooks/.
      7. Optionally run `pre-commit run --all-files` (smoke test).

    IMPORTANT: This file is intentionally ASCII-only. Windows PowerShell 5.1
    reads .ps1 files without a BOM as the system ANSI codepage (CP1251 on
    Russian Windows). UTF-8 Cyrillic in comments would be misinterpreted as
    garbage and break the parser. Keep all source ASCII-only; localized
    documentation lives in docs/pre-commit.md.

    IMPORTANT: Repository root is derived from $PSScriptRoot, NOT from
    `git rev-parse --show-toplevel`. PowerShell 5.1 decodes native command
    stdout using the console codepage (CP866 on Russian Windows), which
    garbles non-ASCII paths returned by git. Deriving the root from the
    script location avoids the codepage round-trip entirely.

    Requirements:
      - Windows PowerShell 5.1+ or PowerShell 7+
      - git and cargo on PATH
      - Python 3.8+ (any of: `py -3`, `python`, `python3`)

.PARAMETER Force
    Reinstall pre-commit even if it is already on PATH.
    Useful after switching Python versions or a broken pipx venv.

.PARAMETER Verify
    Run `pre-commit run --all-files` immediately after install.
    The first clippy run may take a few minutes -- that is expected.

.PARAMETER Uninstall
    Remove .git/hooks/pre-commit and .git/hooks/pre-push.
    The pre-commit framework itself is not removed.

.EXAMPLE
    .\scripts\install-hooks.ps1

.EXAMPLE
    .\scripts\install-hooks.ps1 -Verify

.EXAMPLE
    .\scripts\install-hooks.ps1 -Uninstall

.NOTES
    Documentation: docs/pre-commit.md
#>

[CmdletBinding()]
param(
    [switch]$Force,
    [switch]$Verify,
    [switch]$Uninstall
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ============================================================
# Output helpers
# ============================================================

function Write-Step {
    param([Parameter(Mandatory)][string]$Message)
    Write-Host "==> $Message" -ForegroundColor Cyan
}

function Write-Ok {
    param([Parameter(Mandatory)][string]$Message)
    Write-Host "    [ok] $Message" -ForegroundColor Green
}

function Write-Warn {
    param([Parameter(Mandatory)][string]$Message)
    Write-Host "    [warn] $Message" -ForegroundColor Yellow
}

function Write-Fail {
    param([Parameter(Mandatory)][string]$Message)
    Write-Host "    [fail] $Message" -ForegroundColor Red
}

function Write-Dim {
    param([Parameter(Mandatory)][string]$Message)
    Write-Host "    $Message" -ForegroundColor DarkGray
}

function Test-Command {
    param([Parameter(Mandatory)][string]$Name)
    return $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

# $ErrorActionPreference = 'Stop' does NOT affect native command exit codes:
# PowerShell historically treats them as successful regardless. Wrap explicitly.
function Invoke-Native {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [string[]]$Arguments = @()
    )
    $display = "$FilePath $($Arguments -join ' ')"
    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "command failed (exit $LASTEXITCODE): $display"
    }
}

# ============================================================
# 1. Sanity
# ============================================================

if (-not (Test-Command 'git')) {
    Write-Fail "git not found on PATH"
    exit 1
}

# Derive repo root from the script's own location.
#
# Do NOT use `git rev-parse --show-toplevel`: PowerShell 5.1 decodes native
# command stdout through the console codepage (CP866 on Russian Windows),
# which turns "D:\Парсер\kamtur" into "D:\╨Я╨░╤А╤Б╨╡╤А\kamtur".
# $PSScriptRoot is a proper .NET (UTF-16) string, immune to codepage issues.
$scriptDir = $PSScriptRoot
if ([string]::IsNullOrEmpty($scriptDir)) {
    Write-Fail "cannot determine script directory (PSScriptRoot is empty)."
    Write-Dim "invoke as: .\scripts\install-hooks.ps1  (not 'iex' or '-Command')"
    exit 1
}
$repoRoot = Split-Path -Parent $scriptDir

# Sanity: the parent of scripts/ must be a git worktree. `.git` is a
# directory in normal clones and a file in worktrees / submodules --
# Test-Path returns true for both.
if (-not (Test-Path -LiteralPath (Join-Path $repoRoot '.git'))) {
    Write-Fail "not a git repository: $repoRoot"
    Write-Dim "expected scripts/ to live directly under the repo root"
    exit 1
}

# `-LiteralPath` skips globbing -- matters if the repo path ever contains
# characters like `[` or `?`.
Set-Location -LiteralPath $repoRoot
Write-Step "repository: $repoRoot"

if (-not (Test-Command 'cargo')) {
    Write-Fail "cargo not found on PATH. Install rustup: https://rustup.rs"
    exit 1
}
Write-Ok "cargo: $((& cargo --version))"

$configFile = Join-Path $repoRoot '.pre-commit-config.yaml'
if (-not (Test-Path -LiteralPath $configFile)) {
    Write-Fail ".pre-commit-config.yaml not found in $repoRoot"
    exit 1
}

# ============================================================
# 2. Uninstall
# ============================================================

if ($Uninstall) {
    Write-Step "removing git hooks"
    $removed = 0
    foreach ($hook in @('pre-commit', 'pre-push')) {
        $path = Join-Path $repoRoot ".git/hooks/$hook"
        if (Test-Path -LiteralPath $path) {
            Remove-Item -LiteralPath $path -Force
            Write-Ok "removed .git/hooks/$hook"
            $removed++
        } else {
            Write-Warn ".git/hooks/$hook not present"
        }
    }
    Write-Host ""
    Write-Host "Removed hooks: $removed. pre-commit framework is still installed." `
        -ForegroundColor Green
    exit 0
}

# ============================================================
# 3. pre-commit: find or install
# ============================================================

Write-Step "locating pre-commit"

$preCommit = $null

if (-not $Force -and (Test-Command 'pre-commit')) {
    $version = (& pre-commit --version) -replace '^pre-commit\s+', ''
    $preCommit = 'pre-commit'
    Write-Ok "found on PATH: $version"
} else {
    if ($Force) {
        Write-Warn "-Force: reinstalling pre-commit"
    } else {
        Write-Warn "not on PATH, installing"
    }

    # ----- Python -----
    # Order: `py -3` (Windows launcher) -> `python` -> `python3`.
    # `py -3` is checked for success: the launcher is always present with
    # a Python installer, but Python 3 itself may not be installed.
    $pythonExe = $null
    $pythonArgs = @()

    if (Test-Command 'py') {
        & py -3 --version *> $null
        if ($LASTEXITCODE -eq 0) {
            $pythonExe = 'py'
            $pythonArgs = @('-3')
        }
    }
    if (-not $pythonExe -and (Test-Command 'python')) {
        & python --version *> $null
        if ($LASTEXITCODE -eq 0) { $pythonExe = 'python' }
    }
    if (-not $pythonExe -and (Test-Command 'python3')) {
        & python3 --version *> $null
        if ($LASTEXITCODE -eq 0) { $pythonExe = 'python3' }
    }

    if (-not $pythonExe) {
        Write-Fail "Python 3.8+ not found. Install with one of:"
        Write-Dim "  winget install Python.Python.3.12"
        Write-Dim "  https://www.python.org/downloads/"
        exit 1
    }

    $pyVer = (& $pythonExe @pythonArgs --version 2>&1) -join ' '
    Write-Ok "Python: $pythonExe $($pythonArgs -join ' ') -- $pyVer"

    # ----- pipx (preferred) -----
    $installedViaPipx = $false
    if (Test-Command 'pipx') {
        Write-Dim "installing via pipx (isolated environment)..."
        Invoke-Native -FilePath 'pipx' -Arguments @('install', '--force', 'pre-commit')
        # pipx shims usually live in %USERPROFILE%\.local\bin. The current
        # session's PATH may not know about it -- add it explicitly.
        $pipxBin = Join-Path $env:USERPROFILE '.local\bin'
        if (Test-Path -LiteralPath (Join-Path $pipxBin 'pre-commit.exe')) {
            $env:PATH = "$pipxBin;$env:PATH"
        }
        $installedViaPipx = $true
    }

    # ----- pip --user (fallback) -----
    if (-not (Test-Command 'pre-commit')) {
        if (-not $installedViaPipx) {
            Write-Warn "pipx not found, falling back to pip install --user"
            Write-Dim "for a cleaner setup: py -3 -m pip install --user pipx; pipx install pre-commit"
        }
        $pipArgs = $pythonArgs + @(
            '-m', 'pip', 'install', '--user', '--upgrade', 'pre-commit'
        )
        Invoke-Native -FilePath $pythonExe -Arguments $pipArgs

        # `pip install --user` writes the shim to
        # %APPDATA%\Python\Python3XX\Scripts. Walk every discovered version
        # and take the first one that contains pre-commit.exe.
        $userRoot = Join-Path $env:APPDATA 'Python'
        if (Test-Path -LiteralPath $userRoot) {
            $found = Get-ChildItem -LiteralPath $userRoot -Directory `
                    -ErrorAction SilentlyContinue |
                ForEach-Object { Join-Path $_.FullName 'Scripts' } |
                Where-Object { Test-Path -LiteralPath (Join-Path $_ 'pre-commit.exe') } |
                Select-Object -First 1
            if ($found) {
                $env:PATH = "$found;$env:PATH"
            }
        }
    }

    if (-not (Test-Command 'pre-commit')) {
        Write-Fail "pre-commit installed, but not visible on PATH in this session."
        Write-Warn "Close and reopen the terminal, then run this script again."
        Write-Dim "Tip: add the Scripts folder to your User PATH permanently -- see docs/pre-commit.md"
        exit 1
    }

    $version = (& pre-commit --version) -replace '^pre-commit\s+', ''
    $preCommit = 'pre-commit'
    Write-Ok "installed: $version"
}

# ============================================================
# 4. Validate config
# ============================================================

Write-Step "validating .pre-commit-config.yaml"
Invoke-Native -FilePath $preCommit `
    -Arguments @('validate-config', '.pre-commit-config.yaml')
Write-Ok "config is valid"

# ============================================================
# 5. Register hooks
# ============================================================

Write-Step "registering git hooks"

# `--install-hooks` pre-clones hook environments (pre-commit-hooks repo
# into ~/.cache/pre-commit). Without it the very first commit stalls
# while cloning the hook repository.
Invoke-Native -FilePath $preCommit -Arguments @('install', '--install-hooks')
Write-Ok ".git/hooks/pre-commit"

Invoke-Native -FilePath $preCommit -Arguments @(
    'install', '--hook-type', 'pre-push', '--install-hooks'
)
Write-Ok ".git/hooks/pre-push"

# ============================================================
# 6. Optional: smoke test
# ============================================================

if ($Verify) {
    Write-Step "smoke test: pre-commit run --all-files"
    Write-Dim "the first clippy run may take a few minutes -- one-time cost"

    & $preCommit 'run' '--all-files'
    if ($LASTEXITCODE -ne 0) {
        Write-Warn "some hooks failed. Usually formatting or clippy."
        Write-Warn "Fix, stage, and re-run: pre-commit run --all-files"
        exit $LASTEXITCODE
    }
    Write-Ok "all hooks passed"
}

# ============================================================
# Done
# ============================================================

Write-Host ""
Write-Host "Done." -ForegroundColor Green
Write-Host ""
Write-Host "Next steps:" -ForegroundColor White
Write-Host "  git commit ...                          -- fmt + clippy on staged files"
Write-Host "  git push ...                            -- tests before push"
Write-Host "  pre-commit run --all-files              -- all hooks manually"
Write-Host "  pre-commit run cargo-clippy             -- single hook"
Write-Host "  pre-commit autoupdate                   -- bump hook versions"
Write-Host "  .\scripts\install-hooks.ps1 -Uninstall  -- remove hooks"
Write-Host ""
Write-Host "Documentation: docs/pre-commit.md" -ForegroundColor DarkGray
