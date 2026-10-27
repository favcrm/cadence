# CAD-1334 independent nextest acceptance-check contract

The implementation author must not edit or weaken this contract. Add one executable acceptance check at the CI test-runner boundary; do not substitute source-text assertions or a fake runner for behavior checks.

## Positive case

Invoke the exact entry point CI will use, with the pinned `scripts/cadence-nextest` binary and an isolated temporary Cargo fixture/workspace. Assert the actual invocation covers the exact integration target set emitted by `scripts/result-test-args`, plus `--lib` and `--bins`, with `--features test-seam`. The test fixture must include a real test in each applicable selection category so the check proves they execute, rather than merely checking forwarded arguments. Preserve the runner's real HOME/XDG/TMPDIR isolation and `CADENCE_SUITE_LOCK` behavior; have a fixture test verify the isolated environment and that the suite lock is held while tests execute. At the test-process boundary, assert the caller's `CARGO_TARGET_DIR` is not inherited and `CARGO_BUILD_TARGET_DIR` equals the original absolute temporary `cargo-target` path supplied to the runner. Use temporary paths only and do not invoke the full repository suite.

## Refusal case

Through the same real CI entry point and pinned runner, request a nonexistent test target (or otherwise create an invalid/empty selected-test inventory). Assert the command exits nonzero and reports the selection/discovery failure. Also cover a selected target that discovers zero tests and assert it is refused. Neither case may be converted to a skip or success by an empty inventory, missing target, test filter, or no-tests override.

The check passes only if the positive case executes the requested fixture tests and both bad cases fail closed. It must exercise the runner/gating path used by CI, not a duplicate implementation of its selection logic. Keep all generated manifests, binaries, lock files, and logs under a temporary directory and clean them on exit.
