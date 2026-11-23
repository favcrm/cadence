#!/bin/bash
# CAD-1085 installer entry refusals.
#
# The owner binary is also the pinned installer (`--owner-launched-install`).
# Two entry conditions must refuse before the fixed production profile, the
# capsule or any supervisor socket is touched:
#   1. the installer mode requires root real/effective/saved uid;
#   2. the owner CLI accepts no unknown argument (so no caller-shaped argv can
#      reach the installer dispatch).
#
# Usage: installer-entry-refusals-20261027.sh <cadence-runtime-owner> [unprivileged-uid]
set -euo pipefail

BIN="${1:?path to cadence-runtime-owner}"
UNPRIV="${2:-1000}"

if [ ! -x "$BIN" ]; then
  echo "binary is not executable: $BIN" >&2
  exit 1
fi

set +e
NONROOT_OUT=$(/usr/bin/setpriv --reuid "$UNPRIV" --regid "$UNPRIV" --clear-groups \
  "$BIN" --owner-launched-install 2>&1)
NONROOT_STATUS=$?
ARGV_OUT=$("$BIN" --owner-launched-install extra 2>&1)
ARGV_STATUS=$?
set -e

if [ "$NONROOT_STATUS" -ne 1 ] || [ -n "$NONROOT_OUT" ]; then
  echo "non-root installer invocation was not silently refused (exit $NONROOT_STATUS): $NONROOT_OUT" >&2
  exit 1
fi
if [ "$ARGV_STATUS" -ne 1 ] || ! grep -q 'unknown argument' <<<"$ARGV_OUT"; then
  echo "unknown installer argument was not refused (exit $ARGV_STATUS): $ARGV_OUT" >&2
  exit 1
fi

echo 'installer entry refusals: ok'
