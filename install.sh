#!/usr/bin/env bash
set -euo pipefail

# Build and install jj-plan as `jj`.
#
# Usage:
#   ./install.sh
#   ./install.sh --bin-dir ~/.local/bin
#   ./install.sh --bin-dir=/usr/local/bin
#   ./install.sh --no-build
#
# Defaults:
#   --bin-dir ~/.local/bin

BIN_DIR="${HOME}/.local/bin"
BUILD=true

usage() {
  cat <<EOF
Usage: ./install.sh [--bin-dir DIR] [--no-build]

Options:
  --bin-dir DIR     Destination directory for the installed \`jj\` binary
                    (default: ${HOME}/.local/bin)
  --no-build        Skip \`cargo build --release\`
  --help, -h        Show this help message
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin-dir)
      if [[ $# -lt 2 ]]; then
        echo "Error: --bin-dir requires a directory argument" >&2
        exit 1
      fi
      BIN_DIR="$2"
      shift 2
      ;;
    --bin-dir=*)
      BIN_DIR="${1#--bin-dir=}"
      shift
      ;;
    --no-build)
      BUILD=false
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

if $BUILD; then
  echo "Building release binary..."
  cargo build --release
fi

SRC_BIN="target/release/jj-plan"
DEST_BIN="${BIN_DIR}/jj"

if [[ ! -f "$SRC_BIN" ]]; then
  echo "Error: ${SRC_BIN} does not exist." >&2
  echo "Run without --no-build, or build first with: cargo build --release" >&2
  exit 1
fi

mkdir -p "$BIN_DIR"
cp "$SRC_BIN" "$DEST_BIN"
chmod 755 "$DEST_BIN"

echo "Installed jj-plan to: ${DEST_BIN}"
echo
echo "Make sure '${BIN_DIR}' appears in your PATH before the real jj binary."
echo "Verify with:"
echo "  jj plan --help"