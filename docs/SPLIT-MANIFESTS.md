# Split manifests and inventory guards

## Current reduced-gate window (CAD-1073)

The legacy integration inventories `tests/split-map*.toml` and their Rust
inventory guard were retired by PR #738. Do not run `scripts/split-map-sync`
to register new behavior tests during this window: its input manifests are
absent. That script is historical tooling, not an active gate.

The live doctor/host source manifest is **`src/doctor/host/split-map.toml`**.
Run `scripts/split-doctor-host --check` to verify its declared items and tests.
The local pre-push plan runs this check; CI must keep running it too. Retiring
integration inventories is not permission to disable this source check.

The compiled `tests/safety_floor.rs` is the executable merge-test floor,
not a replacement for the retired inventory or full behavior coverage.
New behavior tests and their inventory model belong to the approved rebuild
work. CAD-1102 re-declared the current gate as permanent and removed the
reduced-gates marker; add behavior checks per AGENTS.md (CAD-1099), not by
restoring the retired inventories.

## Historical inventories

CAD-562 used `tests/split_map_inventory.rs` to compare the per-binary lists
against `#[test]` functions. CAD-905's `scripts/split-map-sync` maintained
those lists. These contracts are available in Git history; they are not
currently exercised by the reduced test gate.

CAD-621 retired the completed CAD-534 daemon and CAD-535 CLI one-shot
split generators and their manifests. Daemon and CLI source files are
maintained directly; do not recreate those manifests to register new items.
The `PINNED` tables in `scripts/split-integration-tests` and
`scripts/split-board-tests` likewise belong to retired one-shot generators.
