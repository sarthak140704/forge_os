#!/usr/bin/env bash
# Forge OS — macOS install script.
#
# Bootstraps a clean macOS machine to build and run Forge OS end-to-end:
#   - Xcode Command Line Tools (Apple's C toolchain — needed by every Rust build)
#   - Homebrew (only if missing; used to install everything else)
#   - Rust toolchain (rustup + stable + rustfmt + clippy)
#   - Node.js LTS
#   - Python 3 (used by scripts/*.py inspectors)
#   - Frontend npm deps
#   - Cargo fetch
#
# Tauri v2 on macOS uses the system WebKit — no separate WebView2-style install.
#
# Idempotent: skips anything already installed.
#
# Usage:
#     chmod +x ./scripts/install-macos.sh
#     ./scripts/install-macos.sh [--skip-brew] [--skip-node] [--skip-python] [--verify]

set -euo pipefail

SKIP_BREW=0
SKIP_NODE=0
SKIP_PYTHON=0
VERIFY=0

for arg in "$@"; do
    case "$arg" in
        --skip-brew)   SKIP_BREW=1 ;;
        --skip-node)   SKIP_NODE=1 ;;
        --skip-python) SKIP_PYTHON=1 ;;
        --verify)      VERIFY=1 ;;
        -h|--help)
            grep '^#' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *) echo "unknown flag: $arg" >&2; exit 2 ;;
    esac
done

step() {
    printf '\n\033[1;36m==> %s\033[0m\n' "$1"
}
warn() {
    printf '\033[1;33m  ⚠ %s\033[0m\n' "$1"
}
err() {
    printf '\033[1;31m  ✗ %s\033[0m\n' "$1"
}

have() {
    command -v "$1" >/dev/null 2>&1
}

# ---------------------------------------------------------------------------

step "Forge OS — macOS install"
echo "This script installs Xcode CLT, Homebrew, Rust, Node, Python."
echo "It is idempotent — safe to re-run."

# ---- 1. Xcode Command Line Tools ------------------------------------------
step "Xcode Command Line Tools"
if xcode-select -p >/dev/null 2>&1; then
    echo "  present: $(xcode-select -p)"
else
    echo "  triggering interactive installer — a system dialog will appear."
    xcode-select --install || true
    echo "  waiting for install to finish (accept the prompt if it appeared)…"
    until xcode-select -p >/dev/null 2>&1; do
        sleep 15
        echo "    still waiting…"
    done
    echo "  installed: $(xcode-select -p)"
fi

# ---- 2. Homebrew ----------------------------------------------------------
step "Homebrew"
if [[ $SKIP_BREW -eq 1 ]]; then
    echo "  --skip-brew set — skipping."
elif have brew; then
    echo "  brew: $(brew --version | head -n 1)"
else
    echo "  installing Homebrew (non-interactive)…"
    NONINTERACTIVE=1 /bin/bash -c \
        "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
    # Apple Silicon default install location:
    if [[ -d /opt/homebrew/bin && ":$PATH:" != *":/opt/homebrew/bin:"* ]]; then
        eval "$(/opt/homebrew/bin/brew shellenv)"
    fi
fi

# ---- 3. Rust toolchain ----------------------------------------------------
step "Rust toolchain"
if have rustup; then
    echo "  rustup: $(rustup --version)"
else
    echo "  installing rustup (default host + stable toolchain)…"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --default-toolchain stable --profile default
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi
rustup update stable
rustup component add rustfmt clippy 2>/dev/null || true

if ! have cargo; then
    err "cargo not on PATH — open a new shell or 'source \$HOME/.cargo/env'."
    exit 1
fi
echo "  cargo: $(cargo --version)"

# ---- 4. Node.js LTS -------------------------------------------------------
step "Node.js LTS"
if [[ $SKIP_NODE -eq 1 ]]; then
    echo "  --skip-node set — skipping."
elif have node; then
    echo "  node: $(node --version)"
else
    if have brew; then
        brew install node
    else
        warn "no brew — install Node from https://nodejs.org/ manually."
    fi
fi

# ---- 5. Python 3 ----------------------------------------------------------
step "Python 3 (for scripts/*.py inspectors)"
if [[ $SKIP_PYTHON -eq 1 ]]; then
    echo "  --skip-python set — skipping."
elif have python3; then
    echo "  python3: $(python3 --version)"
else
    if have brew; then
        brew install python
    else
        warn "no brew — install Python 3 manually or use the system one."
    fi
fi

# ---- 6. Frontend deps -----------------------------------------------------
step "Frontend npm install"
here="$(cd "$(dirname "$0")" && pwd)"
frontend="$here/../apps/forge-desktop/frontend"
if [[ -d $frontend ]]; then
    if have npm; then
        (cd "$frontend" && npm install --no-audit --no-fund)
    else
        warn "npm not on PATH — skip. Re-run after opening a new shell."
    fi
else
    warn "frontend dir not found at $frontend — skip."
fi

# ---- 7. Cargo fetch -------------------------------------------------------
step "Cargo fetch (populate crate cache)"
(cd "$here/.." && cargo fetch)

# ---- 8. Optional: verify --------------------------------------------------
if [[ $VERIFY -eq 1 ]]; then
    step "Verify: cargo check --workspace"
    (cd "$here/.." && cargo check --workspace --tests --examples)
fi

step "Done"
cat <<EOM
Next steps:
  1. Open a NEW shell so PATH updates apply.
  2. Set an LLM key, e.g.:
       export GROQ_API_KEY='<your-key>'
  3. Boot the desktop app:
       cd apps/forge-desktop
       node ./frontend/node_modules/@tauri-apps/cli/tauri.js dev \\
           --config ./src-tauri/tauri.conf.json
  4. Optional — build the headless CLI:
       cargo install --path apps/forge-cli
       forge --help
  5. Optional — VS Code extension:
       cd apps/forge-vscode && npm install && npm run compile   # then press F5 in VS Code
EOM
