CAD-611 measurement uses a real Chromium tab and a private 420-issue / 12-agent / 160-job test stack. It excludes a ten-second warmup, records sixty idle seconds, then records twenty issue-title changes three seconds apart during sixty busy seconds. It counts wire-encoded finite response bytes (including headers), and streaming chunk bytes. Requests are reported per path. SSE headers land during warmup. No production endpoint is accepted; fixture boards use 3110–3198 and the owned Chrome uses 3199.

The **targets**, not measured results, are idle <50,000 bytes/min and busy <2,000,000 bytes/min for this fixed scenario. The latter is a proposed ceiling to make acceptance reproducible; it must be reviewed with the measured before/after result. The operator report (41 requests / 5 MB / about70s) is historical observation, not this fixture's baseline.

Only an admitted runner may execute this procedure. Local external lanes cannot acquire a native identity or bypass a refused slot. Both UI compilation and the filtered Rust fixture require the project's admission. Existing CI runs the deterministic regression tests; this opt-in browser measurement has not run as part of those checks.

For each tree, use a private `/tmp/<lane>` root, pass RUSTUP_HOME/CARGO_HOME through when isolating HOME/XDG/TMPDIR, and set CARGO_BUILD_JOBS=4 and CADENCE_SUITE_LOCK. Build that tree's UI with its lockfile. Have the admitted runner execute:

```sh
CADENCE_BUDGET_DIST=/absolute/tree/ui/dist \
CADENCE_BUDGET_ENDPOINT_FILE=/tmp/<lane>/endpoint \
rtk proxy cargo test --features test-seam --test board_read_model \
  board_live_measurement_fixture -- --ignored --nocapture --test-threads 2
```

While that admitted fixture runs, execute the harness using Node22+ and installed Chrome:

```sh
node scripts/measurements/board-live.mjs /tmp/<lane>/endpoint /tmp/<lane>/after.json
```

Each run requires fresh endpoint and `.busy` trigger filenames. The harness writes the busy trigger after idle measurement. The fixture ends itself; the harness signals only its own Chrome child and removes only its own temporary profile. Close the two commands normally if a run fails; never signal an unrelated process. If3199 is occupied, reschedule the measurement.

The pinned baseline is `e85b5a88663f07c2c88bd42907527ad44fce5248`. To measure it, make an isolated checkout of that exact revision, preserve its head SHA, and apply **only** these test-instrumentation changes from the candidate: the `free_port` helper, the `CADENCE_BUDGET_DIST` option in `start_ui`, and the appended section starting `// CAD-611 MEASUREMENT FIXTURE`. Copy the standalone harness unchanged. Save that instrumentation diff with `before.json`. It makes the fixture/ports/dist identical without changing either baseline's server/cache/App behavior. Build the baseline UI separately and point its fixture at that dist. Record both git heads, instrumentation diff, command receipts and JSON output. Do not declare CAD-611 budget acceptance complete until both trees have real measured output.
