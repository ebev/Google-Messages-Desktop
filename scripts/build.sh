#!/usr/bin/env bash
#
# Build release bundles for Google Messages for Desktop (macOS & Linux).
#
# Usage:
#   ./scripts/build.sh                    # native target for this OS/arch
#   ./scripts/build.sh <target-triple>    # cross-compile to another target
#
# Supported targets:
#   macOS:  aarch64-apple-darwin only (Apple Silicon — this app ships no
#           x86_64-apple-darwin build)
#   Linux:  x86_64-unknown-linux-gnu, aarch64-unknown-linux-gnu
#
# Windows targets are built with scripts/build.ps1 on a Windows host — this
# script only cross-compiles within the OS it runs on, matching how these
# bundles actually get distributed (native bundle formats per OS).
#
# Cross-arch Linux builds need the matching cross C toolchain + target-arch
# WebKitGTK dev libs; scripts/setup-dev.sh installs these on apt-based
# distros. Run it first if a cross build fails with a missing linker/library.
set -euo pipefail

info() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
err()  { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OS="$(uname -s)"

[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env" || true
if ! command -v cargo >/dev/null 2>&1; then
  err "cargo not found. Run scripts/setup-dev.sh first."
  exit 1
fi

HOST_TRIPLE="$(rustc -vV | awk '/^host:/ { print $2 }')"

case "$OS" in
  Darwin)
    ALLOWED_TARGETS=(aarch64-apple-darwin)
    ;;
  Linux)
    ALLOWED_TARGETS=(x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu)
    ;;
  *)
    err "Unsupported OS: $OS. Use scripts/build.ps1 on Windows."
    exit 1
    ;;
esac

TARGET="${1:-$HOST_TRIPLE}"

valid=0
for t in "${ALLOWED_TARGETS[@]}"; do
  [ "$t" = "$TARGET" ] && valid=1
done
if [ "$valid" -ne 1 ]; then
  err "Unsupported target '$TARGET' on $OS. Supported: ${ALLOWED_TARGETS[*]}"
  if [ "$OS" = "Darwin" ]; then
    err "(This app ships Apple Silicon builds only; there is no x86_64-apple-darwin target.)"
  fi
  exit 1
fi

info "Building for $TARGET (host is $HOST_TRIPLE)"

if ! rustup target list --installed | grep -qx "$TARGET"; then
  info "Installing rust target $TARGET…"
  rustup target add "$TARGET"
fi

# Wire up the cross C toolchain + pkg-config search path when targeting a
# foreign architecture on Linux. These env var names/values match the
# packages scripts/setup-dev.sh installs via apt (gcc-<triple>,
# libwebkit2gtk-4.1-dev:<arch>, etc.) — see that script for the package list.
if [ "$OS" = "Linux" ] && [ "$TARGET" != "$HOST_TRIPLE" ]; then
  case "$TARGET" in
    aarch64-unknown-linux-gnu) CROSS_TRIPLE=aarch64-linux-gnu ;;
    x86_64-unknown-linux-gnu) CROSS_TRIPLE=x86_64-linux-gnu ;;
    *) CROSS_TRIPLE="" ;;
  esac
  if [ -n "$CROSS_TRIPLE" ]; then
    if ! command -v "${CROSS_TRIPLE}-gcc" >/dev/null 2>&1; then
      err "${CROSS_TRIPLE}-gcc not found."
      err "Run scripts/setup-dev.sh to install the cross toolchain for this target."
      exit 1
    fi
    CARGO_ENV_TARGET="$(echo "$TARGET" | tr '[:lower:]-' '[:upper:]_')"
    CC_ENV_TARGET="$(echo "$TARGET" | tr '-' '_')"
    export "CARGO_TARGET_${CARGO_ENV_TARGET}_LINKER=${CROSS_TRIPLE}-gcc"
    export "CC_${CC_ENV_TARGET}=${CROSS_TRIPLE}-gcc"
    export "CXX_${CC_ENV_TARGET}=${CROSS_TRIPLE}-g++"
    export PKG_CONFIG_ALLOW_CROSS=1
    export PKG_CONFIG_PATH="/usr/lib/${CROSS_TRIPLE}/pkgconfig"
    export PKG_CONFIG_LIBDIR="/usr/lib/${CROSS_TRIPLE}/pkgconfig"
    info "Cross toolchain: ${CROSS_TRIPLE}-gcc"
  fi
fi

cd "$PROJECT_DIR"
npm run tauri -- build --target "$TARGET"

BUNDLE_DIR="$PROJECT_DIR/src-tauri/target/$TARGET/release/bundle"
info "Done. Bundles in: $BUNDLE_DIR"
[ -d "$BUNDLE_DIR" ] && find "$BUNDLE_DIR" -maxdepth 2 -type f
