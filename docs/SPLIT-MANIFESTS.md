# Split manifests and inventory guards

CAD-562 treats the retained split test manifests as test inventories:
`tests/split-map*.toml` list the tests assigned to each integration binary.
`tests/split_map_inventory.rs` checks these inventories against the source,
so adding, moving or deleting a listed test requires updating its manifest.

The doctor/host manifest, `src/doctor/host/split-map.toml`, is also active.
Its test inventory is checked by the same Rust guard, and CI runs
`scripts/split-doctor-host --check` to verify its declared items and tests.
Keep this script and manifest synchronized when changing doctor/host code.
Their retirement would require a separate decision about those CI consumers.

CAD-621 retired the completed CAD-534 daemon and CAD-535 CLI one-shot
split generators and their manifests. The old `scripts/split-daemon`,
`scripts/split-main`, `src/daemon/split-map.toml` and
`src/cli/split-map.toml` are available in Git history only. Daemon and CLI
source files are maintained directly; do not recreate these manifests or
regenerate the completed splits to register new items.

## Registering tests (CAD-905)

The `#[test]` fns in `tests/<binary>.rs` are the source of truth for the
`tests = [...]` lists in `tests/split-map.toml` and
`tests/split-map-board.toml`. After adding, moving or deleting an
integration test, run `scripts/split-map-sync`: it appends missing names and
drops stale ones in the right `[binaries.<binary>]` section. `scripts/split-map-sync --check`
(also the second step of `scripts/pre-push`) fails in well under a second,
naming the file, the section and the names to add or remove. The Rust guard
`split_map_inventory` stays the CI contract, so coverage is still
exactly-once. The `PINNED` tables in `scripts/split-integration-tests` and
`scripts/split-board-tests` belong to the retired one-shot generators (their
source files no longer exist); never register a new test there.
