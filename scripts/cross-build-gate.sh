#!/usr/bin/env bash
# CAD-1540: body of the `cross-build (required)` gate job.
#
# CI cannot require `cross-build` directly. The matrix is gated by
# `change-scope`, so on a docs-only change it never runs, and a required
# context that is skipped or absent blocks merges instead of guarding them.
# This script is what the always-running gate job executes: it passes only
# when the matrix passed or was legitimately skipped, and refuses anything
# else, so the required check reports the matrix's real outcome.
#
# Usage: CROSS_BUILD_RESULT=<result> scripts/cross-build-gate.sh
set -euo pipefail

result="${CROSS_BUILD_RESULT:?CROSS_BUILD_RESULT must be set to the cross-build matrix result}"

case "$result" in
  success)
    echo "cross-build: passed"
    ;;
  skipped)
    echo "cross-build: nothing to build for this change scope (docs-only change)"
    ;;
  *)
    echo "cross-build: ${result} — a required check cannot pass on that" >&2
    exit 1
    ;;
esac
