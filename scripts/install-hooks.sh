#!/usr/bin/env bash
# ============================================================
# Pre-commit hooks installer for kamtur (Linux/macOS/Git Bash/WSL)
# ============================================================
#
# Mirrors scripts/install-hooks.ps1. Idempotent.
#
# Requirements: git, cargo, Python 3.8+ (pipx recommended).
#
# NOTE: This script is intentionally ASCII-only. Same rationale as the
# PowerShell twin -- localized documentation lives in docs/pre-commit.md.
#
# Documentation: docs/pre-commit.md

set -euo pipefail

# ------------------------------------------------------------
# Colors
# ------------------------------------------------------------

if [[ -t 1 ]]; then
    C_RESET='\033[0m'
    C_CYAN='\033[36m'
    C_GREEN='\033[32m'
    C_YELLOW='\033[33m'
    C_RED='\033[31m'
    C_DIM='\033[2m'
else
    C_RESET=''; C_CYAN=''; C_GREEN=''; C_YELLOW=''; C_RED=''; C_DIM=''
fi

step() { printf "${C_CYAN}==> %s${C_RESET}\n" "$*"; }
ok()   { printf "    ${C_GREEN}[ok]${C_RESET} %s\n" "$*"; }
warn() { printf "    ${C_YELLOW}[warn]${C_RESET} %s\n" "$*"; }
fail() { printf "    ${C_RED}[fail]${C_RESET} %s\n" "$*"; }
dim()  { printf "${C_DIM}    %s${C_RESET}\n" "$*"; }
die()  { fail "$*"; exit 1; }

has() { command -v "$1" >/dev/null 2>&1; }

usage() {
    cat <<'EOF'
Pre-commit hooks installer for kamtur (Linux/macOS/Git Bash/WSL).

Usage:
    ./scripts/install-hooks.sh [OPTIONS]

Options:
    --force       Reinstall pre-commit even if it is already on PATH
    --verify      Run `pre-commit run --all-files` after install
    --uninstall   Remove .git/hooks/{pre-commit,pre-push}
    -h, --help    Show this help

Requirements:
    git, cargo, Python 3.8+ (pipx strongly recommended).

Examples:
    ./scripts/install-hooks.sh
    ./scripts/install-hooks.sh --verify
    ./scripts/install-hooks.sh --uninstall
EOF
}

# ------------------------------------------------------------
# Args
# ------------------------------------------------------------

FORCE=0
VERIFY=0
UNINSTALL=0

for arg in "$@"; do
    case "$arg" in
        --force)     FORCE=1 ;;
        --verify)    VERIFY=1 ;;
        --uninstall) UNINSTALL=1 ;;
        -h|--help)   usage; exit 0 ;;
        *)           die "unknown argument: $arg (try --help)" ;;
    esac
done

# ------------------------------------------------------------
# 1. Sanity
# ------------------------------------------------------------

has git || die "git not found on PATH"

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null) \
    || die "current directory is not a git repository"
cd "$REPO_ROOT"
step "repository: $REPO_ROOT"

has cargo || die "cargo not found on PATH. Install rustup: https://rustup.rs"
ok "cargo: $(cargo --version)"

CONFIG_FILE="$REPO_ROOT/.pre-commit-config.yaml"
[[ -f "$CONFIG_FILE" ]] || die ".pre-commit-config.yaml not found in $REPO_ROOT"

# ------------------------------------------------------------
# 2. Uninstall
# ------------------------------------------------------------

if [[ $UNINSTALL -eq 1 ]]; then
    step "removing git hooks"
    removed=0
    for hook in pre-commit pre-push; do
        path="$REPO_ROOT/.git/hooks/$hook"
        if [[ -f "$path" ]]; then
            rm -f "$path"
            ok "removed .git/hooks/$hook"
            removed=$((removed + 1))
        else
            warn ".git/hooks/$hook not present"
        fi
    done
    echo
    printf "${C_GREEN}Removed hooks: %d. pre-commit framework is still installed.${C_RESET}\n" \
        "$removed"
    exit 0
fi

# ------------------------------------------------------------
# 3. pre-commit: find or install
# ------------------------------------------------------------

step "locating pre-commit"

if [[ $FORCE -eq 0 ]] && has pre-commit; then
    ok "found on PATH: $(pre-commit --version | sed 's/^pre-commit //')"
else
    if [[ $FORCE -eq 1 ]]; then
        warn "--force: reinstalling pre-commit"
    else
        warn "not on PATH, installing"
    fi

    # ----- Python -----
    if has python3; then
        PY=python3
    elif has python; then
        PY=python
    else
        die "Python 3.8+ not found. Install: apt/dnf/brew install python3"
    fi
    ok "Python: $($PY --version 2>&1)"

    # ----- pipx (preferred) -----
    if has pipx; then
        dim "installing via pipx (isolated environment)..."
        pipx install --force pre-commit
        if [[ -x "$HOME/.local/bin/pre-commit" ]]; then
            export PATH="$HOME/.local/bin:$PATH"
        fi
    fi

    # ----- pip --user (fallback) -----
    if ! has pre-commit; then
        if ! has pipx; then
            warn "pipx not found, falling back to pip install --user"
            dim "for a cleaner setup: python3 -m pip install --user pipx; pipx install pre-commit"
        fi
        "$PY" -m pip install --user --upgrade pre-commit

        # Linux: ~/.local/bin; macOS: ~/Library/Python/X.Y/bin.
        for candidate in "$HOME/.local/bin" "$HOME"/Library/Python/*/bin; do
            if [[ -x "$candidate/pre-commit" ]]; then
                export PATH="$candidate:$PATH"
                break
            fi
        done
    fi

    has pre-commit \
        || die "pre-commit installed, but not on PATH. Reopen the terminal and retry."

    ok "installed: $(pre-commit --version | sed 's/^pre-commit //')"
fi

# ------------------------------------------------------------
# 4. Validate config
# ------------------------------------------------------------

step "validating .pre-commit-config.yaml"
pre-commit validate-config .pre-commit-config.yaml
ok "config is valid"

# ------------------------------------------------------------
# 5. Register hooks
# ------------------------------------------------------------

step "registering git hooks"
pre-commit install --install-hooks
ok ".git/hooks/pre-commit"
pre-commit install --hook-type pre-push --install-hooks
ok ".git/hooks/pre-push"

# ------------------------------------------------------------
# 6. Optional: smoke test
# ------------------------------------------------------------

if [[ $VERIFY -eq 1 ]]; then
    step "smoke test: pre-commit run --all-files"
    dim "the first clippy run may take a few minutes -- one-time cost"

    if ! pre-commit run --all-files; then
        warn "some hooks failed. Usually formatting or clippy."
        warn "Fix, stage, and re-run: pre-commit run --all-files"
        exit 1
    fi
    ok "all hooks passed"
fi

# ------------------------------------------------------------
# Done
# ------------------------------------------------------------

echo
printf "${C_GREEN}Done.${C_RESET}\n\n"
echo "Next steps:"
echo "  git commit ...                          -- fmt + clippy on staged files"
echo "  git push ...                            -- tests before push"
echo "  pre-commit run --all-files              -- all hooks manually"
echo "  pre-commit run cargo-clippy             -- single hook"
echo "  pre-commit autoupdate                   -- bump hook versions"
echo "  ./scripts/install-hooks.sh --uninstall  -- remove hooks"
echo
dim "Documentation: docs/pre-commit.md"
