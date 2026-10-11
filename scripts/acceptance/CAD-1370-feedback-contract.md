# CAD-1370 independent acceptance contract — trusted-base tests feedback scope

The implementation author must not edit or weaken this check. It exercises
the real `scripts/ci-change-scope.py` CLI (`--repo --base --head --event`)
on a synthetic temporary git repository; nothing re-implements the policy.

## Positive

A `pull_request` diff that modifies only *pre-existing, regular, top-level*
`tests/<identifier>.rs` files may report the narrow `tests` scope naming
exactly the Cargo-owned target names of the changed files. `rust` and
`tests` stay `"true"` (the lib/bin baseline still runs; only integration
targets narrow).

## Refusals — must stay `full` (or nonzero exit), never narrowed

- Added, deleted, renamed, symlinked, or mode-changed `tests/*.rs` files.
- A nested `tests/<dir>/main.rs` or any non-top-level test path.
- Any path outside `tests/` (src, `Cargo.toml`, workflow, docs mixed in).
- A diff that also edits the policy file — a PR cannot self-select its
  scope (the policy is loaded from the trusted base, and editing it forces
  `full`).
- A test file whose basename/path/`mod <stem>` is referenced by any other
  tracked base `.rs` source (shared, not independent). Its unreferenced
  sibling must still narrow.
- Every non-PR event — `merge_group`, `push`, `workflow_dispatch`,
  `pull_request_target`, empty/unknown — with the same test-only diff.
  This is also the forged-plan case: the classifier returns `full` under
  `merge_group`, so a narrowed `tests` result is impossible to emit.
- Malformed or zero base/head SHAs.

## Runner refusals — real `scripts/run-result-tests --test-selection FILE`

Invoked directly at the lane root (no compile, no nextest invocation).
Each case must exit nonzero AND print its own `::error::` guard message —
a generic metadata failure does not count as the guard. Every probe runs
with `PATH=/nonexistent` via absolute `/bin/bash`: the runner refuses
ineligible requests before metadata, tool install or builds, so reaching
the guard message is itself proof no external tool ran.

- `GITHUB_EVENT_NAME=merge_group` + a selection file → refused
  (`pull_request feedback only`): a forged narrowed scope cannot ride the
  queue. This is the one required bad case.
- `pull_request` + an empty file → refused (`selected no targets`).
- `pull_request` + a nonexistent selection file → refused (`selection
  file does not exist`).

Unknown-identifier membership needs the cargo metadata inventory, so it is
not probed here; the classifier-side refusal cases (no name escapes the
plan) plus the implementer's own tests cover it. The lib/bin suites and
the JUnit zero-target guards run in every mode and are unchanged
(CAD-1334 contract).

## Workflow handoff — text assertion

`ci.yml` must carry the trusted-base `targets` output into the `test` job
and call `run-result-tests --test-selection` from it — never an ambient
env or a PR-provided path. Asserted by literal presence of the
`steps.scope.outputs.targets` output, the
`needs.change-scope.outputs.targets` env, and the `--test-selection`
invocation. Static only; the reviewer's read of the diff is the check.

## Non-negotiables

- Temporary dirs only; no production paths, no repo state, no Cargo
  compile, no network.
- The real policy executable is invoked once per case; the result is read
  straight from its JSON, not normalized through multiple interfaces.
