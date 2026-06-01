#!/usr/bin/env bash
#
# Developer environment setup for Google Messages for Desktop (macOS & Linux).
#
# Installs: Rust (rustup), Node.js, the platform build dependencies Tauri needs,
# then runs `npm install`. Safe to re-run — anything already present is skipped.
#
# Usage:  ./scripts/setup-dev.sh
#
set -euo pipefail

info() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33mwarn:\033[0m %s\n' "$1"; }
err()  { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OS="$(uname -s)"

install_linux_deps() {
  info "Linux detected — installing Tauri build dependencies"
  if command -v apt-get >/dev/null 2>&1; then
    sudo apt-get update
    sudo apt-get install -y \
      libwebkit2gtk-4.1-dev build-essential curl wget file \
      libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
  elif command -v dnf >/dev/null 2>&1; then
    sudo dnf install -y \
      webkit2gtk4.1-devel openssl-devel curl wget file \
      libappindicator-gtk3-devel librsvg2-devel
    sudo dnf group install -y "C Development Tools and Libraries" \
      || sudo dnf groupinstall -y "Development Tools" || true
  elif command -v pacman >/dev/null 2>&1; then
    sudo pacman -Syu --needed --noconfirm \
      webkit2gtk-4.1 base-devel curl wget file openssl \
      libappindicator-gtk3 librsvg
  elif command -v zypper >/dev/null 2>&1; then
    sudo zypper install -y \
      webkit2gtk3-devel libopenssl-devel curl wget file \
      libappindicator3-1 librsvg-devel
    sudo zypper install -y -t pattern devel_basis || true
  else
    warn "Unknown package manager. Install the Tauri Linux prerequisites manually:"
    warn "  https://tauri.app/start/prerequisites/"
  fi
}

# 1. Platform build dependencies
case "$OS" in
  Darwin)
    info "macOS detected"
    if xcode-select -p >/dev/null 2>&1; then
      info "Xcode Command Line Tools already installed"
    else
      info "Installing Xcode Command Line Tools (a GUI prompt may appear)…"
      xcode-select --install || true
      warn "Finish the Command Line Tools install if prompted, then re-run this script."
    fi
    ;;
  Linux)
    install_linux_deps
    ;;
  *)
    err "Unsupported OS: $OS. Use scripts/setup-dev.ps1 on Windows."
    exit 1
    ;;
esac

# 2. Rust (rustup)
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env" || true
if command -v cargo >/dev/null 2>&1; then
  info "Rust already installed: $(cargo --version)"
else
  info "Installing Rust via rustup…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env"
fi

# 3. Node.js
if command -v node >/dev/null 2>&1; then
  info "Node.js already installed: $(node --version)"
elif [ "$OS" = "Darwin" ] && command -v brew >/dev/null 2>&1; then
  info "Installing Node.js via Homebrew…"
  brew install node
elif [ "$OS" = "Linux" ]; then
  warn "Node.js not found. Install Node.js LTS via your package manager or nvm:"
  warn "  https://nodejs.org/en/download/package-manager"
  exit 1
else
  err "Node.js not found. Install Node.js LTS from https://nodejs.org and re-run."
  exit 1
fi

# 4. JS dependencies
info "Installing npm dependencies…"
cd "$PROJECT_DIR"
npm install

# 5. Verify the environment is actually usable.
info "Verifying toolchain…"
ok=1
check_tool() {
  if command -v "$1" >/dev/null 2>&1; then
    info "  $1: $("$1" --version 2>/dev/null | head -1)"
  else
    warn "  $1: NOT FOUND on PATH"
    ok=0
  fi
}
check_tool cargo
check_tool rustc
check_tool node
check_tool npm

# A C compiler/linker is required for Rust to link.
if command -v cc >/dev/null 2>&1 || command -v clang >/dev/null 2>&1 || command -v gcc >/dev/null 2>&1; then
  info "  C linker: present"
else
  warn "  C linker: none of cc/clang/gcc found — Rust linking will fail"
  warn "    macOS: run 'xcode-select --install'; Linux: install build-essential / Development Tools"
  ok=0
fi

echo
if [ "$ok" -eq 1 ]; then
  info "Environment looks good."
  info "Start the app with:  npm run dev"
else
  warn "Some checks failed (see above). Fix them, then run: npm run dev"
fi
