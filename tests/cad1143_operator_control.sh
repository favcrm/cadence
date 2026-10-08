#!/usr/bin/env bash
# CAD-1143 destinations-read + prepared-intent — OPERATOR CONTROL (external).
#
# Complete isolated-fixture harness, independently authored — NOT an
# `#[ignore]`d cargo test and never run in-band by the suite. Boots the
# same daemon + counting-stub + install/bind/publish_set fixture as
# `cad1143_destinations_read_acceptance.rs`, then makes the guarded
# `app_publish_destinations_list` and `app_publish_intent_prepare` calls,
# plus forged `app_publish_intent_attach` refusals, through REAL
# operator-origin `client::rpc` calls (the daemon derives the caller from
# SO_PEERCRED + /proc — `proven_operator`, CAD-276 — not a seam assertion).
#
# The driver binary (`examples/cad1143_operator_control.rs`, authored
# with this check, not by the implementer) owns the fixture bring-up and
# the assertion. This wrapper only refuses managed/pane callers up front
# and runs it — nonzero on refusal or on any failed assertion.
#
# RUN — operator-origin only, from an attached operator shell outside
# every pane and managed endpoint:
#     bash tests/cad1143_operator_control.sh
# or directly:
#     cargo run --features test-seam --example cad1143_operator_control
#
# Exit codes: 0 only after the real operator destinations reads pass, a
# completed run prepares with stable identity, and forged/replayed attach
# attempts leave zero queued/authorized sends; 2 on refusal (not an
# operator-origin caller, or the daemon refused/failed). Nothing here
# fakes operator identity, clears env, setsid-detaches, or scrubs
# ancestry — the daemon is the authority and refuses a non-operator peer.
set -euo pipefail

# Refuse the obvious managed/pane caller before we even build. The
# daemon's proven_operator is the real authority, but fail fast so a
# pane run is never mistaken for the operator control.
for v in CADENCE_ALIAS CADENCE_RUNNER_ID; do
  if [ -n "${!v:-}" ]; then
    echo "refusing: $v is set — not an operator-origin shell" >&2
    exit 2
  fi
done

cd "$(dirname "$0")/.."

echo "operator control: building the isolated fixture driver" >&2
echo "(test-seam is a build-flag only; the guarded call itself is" >&2
echo "unscoped — the daemon derives this caller from /proc, real proof)" >&2

exec cargo run --quiet --features test-seam --example cad1143_operator_control
