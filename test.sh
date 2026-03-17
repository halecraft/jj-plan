#!/usr/bin/env bash
set -euo pipefail

# Build release binary and run bats tests with parallel execution.
# Usage:
#   ./test.sh          # build + test (8 parallel jobs)
#   ./test.sh --jobs 4 # build + test (custom parallelism)
#   ./test.sh --no-build # skip build, just run tests

JOBS=8
BUILD=true

for arg in "$@"; do
  case "$arg" in
    --jobs=*) JOBS="${arg#--jobs=}" ;;
    --no-build) BUILD=false ;;
    --help|-h)
      echo "Usage: ./test.sh [--jobs=N] [--no-build]"
      echo "  --jobs=N     parallel jobs (default: 8, requires: brew install parallel)"
      echo "  --no-build   skip cargo build"
      exit 0
      ;;
    *) echo "Unknown argument: $arg"; exit 1 ;;
  esac
done

cd "$(dirname "$0")"

if $BUILD; then
  echo "Building release binary..."
  cargo build --release
fi

echo "Running 138 bats tests (--jobs $JOBS)..."
exec bats jj-plan.bats --jobs "$JOBS"