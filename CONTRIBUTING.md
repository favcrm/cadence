# Contributing to Cadence

Read [project context](docs/START-HERE.md), the affected row of the
[architecture map](docs/ARCHITECTURE.md), and [AGENTS.md](AGENTS.md) before
starting. The source tree contains the contributor references; additional local
session notes and design plans are not required to navigate a fresh clone.

## Start one lane

Fetch `origin`, then separately search all PR states by issue ID and topic:

```bash
git fetch origin
gh pr list -R favcrm/cadence --state all --search "<ISSUE-ID>"
gh pr list -R favcrm/cadence --state all --search "<TOPIC>"
cadence issue start <ISSUE-ID> --base origin/main
```

Stop if another PR already covers the work. Use the printed lane worktree and
its private target directory. Repeat the fetch and both searches before every
push. Native agents obtain identity with `cadence self` and follow their
briefing; an external session must not borrow a native alias or turn token.

## Fast feedback

Use a stable Rust toolchain with rustfmt/clippy, Python 3.11+ for script checks
(the recipe validator uses standard-library `tomllib`), and
Node 22 with the pnpm version declared in [ui/package.json](ui/package.json).
Install UI dependencies with `pnpm -C ui install --frozen-lockfile`.
The [nextest installer](scripts/install-cadence-nextest) and
[runner](scripts/cadence-nextest) pin and verify the test executable.

Cheap navigation and formatting checks run without compiling Rust:

```bash
python3 scripts/check-doc-links.py
python3 -m unittest discover -s scripts -p 'test_check_doc_links.py'
rtk proxy cargo fmt --all -- --check
```

For UI changes, run `pnpm -C ui typecheck`, `pnpm -C ui test:url-state` and
`pnpm -C ui build`. Despite its name, `test:url-state` covers the compiled
frontend test directory. Use `pnpm -C ui test` when that alias is available in
the checked-out package scripts.

Select the affected Rust test binary and a meaningful filter. For example,
after admission and isolation are configured:

```bash
rtk proxy cargo test --locked --features test-seam --test daemon <FILTER> -- --test-threads 2
```

Set `CARGO_BUILD_JOBS=4`. Keep host-wide sccache enabled; only use the documented
one-off `RUSTC_WRAPPER=` diagnostic after a strange build failure. Shared model
changes also require compilation of all affected targets: a narrow behavior
selection cannot find an incompatible initializer in another test crate.

On a shared Cadence host, obtain build/test admission before compilation.
Use an authenticated `cadence build-slot run test -- <command>` when your native
endpoint can hold a slot. Callers without a pane use an authorized project
recipe through `cadence build-slot launch <RECIPE> --project cadence --worktree <LANE>`.
Check `cadence build-slot --help` and the project's configured recipes; recipe
names are project configuration, not universal commands. If identity or recipe
configuration refuses admission, report that blocker to the operator rather
than running an unadmitted build. A suite lock is separate from build admission.

## Fixture isolation and test tiers

Fixture daemons and boards use a short `/tmp/<lane>` root for HOME, XDG paths
and TMPDIR: Unix socket paths cannot exceed 107 bytes. Pass through the real
`RUSTUP_HOME` and `CARGO_HOME` so an isolated HOME can resolve the toolchain.
Set `CADENCE_SUITE_LOCK` to the host's designated suite lock. Direct nextest
invocations require it; the runner acquires the lock and prevents nested locking
by test children. Do not invent a per-lane lock to bypass host serialization.
Use ports 3110–3199 for boards you start and preserve production services.

Choose tests by the changed contract:

- Pure state/decision tests give quick feedback without process startup or waits.
- Focused integration tests prove process, socket, HTTP and persistence wiring.
- CI checks the full test inventory and runs the required suite with `test-seam`.
  Default-feature checks retain real caller-identity coverage; the debug-only
  seam must never appear in release builds. The `ui` feature embeds a previously
  built SPA. Avoid `--all-features`: `e2e` is an opt-in journey tier with its own
  release/Node/browser prerequisites.

Retain real timing budgets and operator-lineage proofs. Enforced gates require
adversarial agent, detached-child, concurrent and forged-field tests first,
including the HTTP peer when applicable. Do not weaken assertions, skip tests
or add retries to obtain a green result. Rerun a loaded-host timeout alone before
concluding it is a product failure.

## Review and delivery

The [review recipe](cadence-review.toml) declares preparation, ordered gates,
isolated/stress reruns and the full suite. The
[CI workflow](.github/workflows/ci.yml) defines required merge-queue checks.
Use `cadence review <PR>` with the configured host suite lock and admitted
resources for a full review. It reads the recipe from the PR's base revision;
a recipe edited on the candidate branch does not override that trust boundary.
`--no-full` is partial feedback, not full-suite evidence.

The full-suite command is `scripts/cadence-nextest --all-targets --features test-seam`;
an isolated case uses `scripts/cadence-nextest --features test-seam --test <TARGET> -- <TEST> --exact`.
These retain pinned checksums, zero retries, non-empty selections and JUnit
evidence. Judge check exit codes through `rtk proxy`, not filtered output.

Update the owning contract and examples with behavior changes. Check navigation
with `python3 scripts/check-doc-links.py`; it uses Git's tracked inventory, so
an ignored local document cannot satisfy a link. It checks paths, not external
sites or heading fragments.

Post the six-field reflection (expected, evidence, cause, correction, lesson,
next) to the ticket. Hand independent review the exact committed head and PR.
Only a passing review pinned to that head permits merge-queue enrollment:
`gh pr merge <PR> -R favcrm/cadence --auto --squash --match-head-commit <REVIEWED-SHA>`.
Disable auto-merge before changing a queued head. The full delivery and
production-safety rules remain in [AGENTS.md](AGENTS.md).
