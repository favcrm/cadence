#!/usr/bin/env bash
set -euo pipefail
kind="$1"
revision="$2"
root="$RUNNER_TEMP/cad611-$kind"
mkdir -p "$root/out" "$root/home" "$root/tmp" "$root/xdg"
git worktree add --detach "$root/tree" "$revision"
if [ "$kind" = baseline ]; then
  git -C "$root/tree" apply "$GITHUB_WORKSPACE/scripts/measurements/baseline-fixture.patch"
fi
git -C "$root/tree" rev-parse HEAD > "$root/out/head.txt"
git -C "$root/tree" diff > "$root/out/instrumentation.diff"
cp "$GITHUB_WORKSPACE/scripts/measurements/board-live.mjs" "$root/out/harness.mjs"
cd "$root/tree/ui"
pnpm install --frozen-lockfile
pnpm build
cd "$root/tree"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export HOME="$root/home" TMPDIR="$root/tmp" XDG_CONFIG_HOME="$root/xdg/config" XDG_STATE_HOME="$root/xdg/state" XDG_CACHE_HOME="$root/xdg/cache"
export CARGO_BUILD_JOBS=4 CADENCE_SUITE_LOCK="$root/lock"
export CADENCE_BUDGET_DIST="$root/tree/ui/dist" CADENCE_BUDGET_ENDPOINT_FILE="$root/endpoint"
cargo test --locked --features test-seam --test board_read_model --no-run > "$root/out/build.log" 2>&1
cargo test --locked --features test-seam --test board_read_model board_live_measurement_fixture -- --ignored --nocapture --test-threads 2 > "$root/out/fixture.log" 2>&1 &
fixture_pid=$!
set +e
node "$root/out/harness.mjs" "$root/endpoint" "$root/out/$kind.json" > "$root/out/browser.log" 2>&1
browser_status=$?
wait "$fixture_pid"
fixture_status=$?
set -e
printf 'browser=%s fixture=%s\n' "$browser_status" "$fixture_status" > "$root/out/exit-status.txt"
[ "$fixture_status" = 0 ]
[ -f "$root/out/$kind.json" ]
if [ "$kind" = candidate ]; then [ "$browser_status" = 0 ]; fi
