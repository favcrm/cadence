<!-- cadence:begin -->
## Cadence-managed agents

This repo may be worked on by cadence-managed agents. If
`CADENCE_ALIAS` is set in your environment: run `cadence self` for
your identity and running turn token, read your briefing at
`.cadence/<group>/BRIEFING-<alias>.md`, report with
`cadence message result <msg-id> --token <turn_id> --text ...`,
and discover peers with `cadence agent list`.
<!-- cadence:end -->

## Delivery workflow

Main moves every few minutes and many sessions work in parallel. Follow
this workflow exactly.

### Before you start, and before every push
- Run `git fetch origin`. Then run
  `gh pr list -R favcrm/cadence --state all --search "<ISSUE-ID>"` as a
  separate command and read its output. If another PR covers the issue,
  stop and report it. Search by topic as well, because duplicates often
  carry a different id.
- Work in the lane worktree that `cadence issue start <ID>` creates,
  never on main.

### Merging: use the merge queue, never `--admin`
- A PR merges only after an independent review passes, pinned to a head
  SHA.
- To enqueue:
  `gh pr merge <n> -R favcrm/cadence --auto --squash --match-head-commit <reviewed-sha>`.
  The queue re-tests the PR on the newest main plus every entry ahead of
  it, so you don't need to rebase first. If the head moved after the
  review, the enqueue is refused.
- If the queue ejects a PR (`failed_checks`), read the failed run. The
  cause is often a semantic clash with a PR that just landed, for example
  a new test that uses a name this PR reserves. Fix it on the branch,
  show a `git range-diff` against the reviewed head (only the rebase plus
  the fix), and enqueue it again.
- Use `--admin` only in a declared emergency, never as routine.

### Checks: trust exit codes, not filtered text
- A shell hook routes commands through `rtk`, which can print "clean" or
  nothing at all when a check really failed. It has hidden a
  `cargo fmt` failure and non-empty git diffs. Run checks as
  `rtk proxy <cmd>` and judge them by exit code:
  `rtk proxy cargo fmt --all -- --check`,
  `rtk proxy cargo clippy --all-targets -- -D warnings`,
  `rtk proxy gh pr checks <n>`, `rtk proxy git diff …`.
- Never skip, ignore or weaken a test or check to get green.

### Local builds and tests
- sccache is the host-wide rustc wrapper (set in `~/.cargo/config.toml`).
  Don't override `RUSTC_WRAPPER`. If a build fails in a strange way, rerun
  it once with `RUSTC_WRAPPER=` to rule sccache out.
- Many lanes share this host. Use `CARGO_BUILD_JOBS=4` and
  `--test-threads 2`, and run only filtered test groups; CI runs the full
  suite.
- Tests isolate HOME, XDG and TMPDIR under a short `/tmp/<lane>` root,
  because unix socket paths are limited to 107 bytes. They set
  `CADENCE_SUITE_LOCK` and pass `RUSTUP_HOME` and `CARGO_HOME` through so
  the toolchain resolves.
- If a test fails with a timeout or "daemon not reachable" while the host
  is loaded, rerun that test alone before treating the failure as real.

### Production safety
- A production daemon runs on this host. Never touch
  `~/.local/state/cadence`, `~/pm` (apart from `cadence issue …`
  commands), port 3010, the installed `~/.local/bin/cadence`, or the
  tailnet.
- Never restart the daemon. The rollout has a single owner; see
  `cadence rollout status` and CAD-236.
- Boards and daemons you start use a temp state dir and a port in
  3110–3199.
- Never signal or kill a process you did not start.

### Shared scratch space
- The session scratchpad is shared between agents. Write only inside your
  own `<scratchpad>/<lane>/` subdirectory, and delete only your own files,
  by name. Never run `rm <scratchpad>/*`.

### Gates and security work
- For any rule the daemon enforces (operator-only actions, "exactly
  once", a gate), write the adversarial test first. That means an agent
  caller, a detached child (`setsid`), concurrent calls, and a forged
  field. Prove the test fails without the guard.
- A board or HTTP path must be at least as strict as the daemon RPC it
  relays, so run the same operator proof on the HTTP peer.
- Restrict actors with allowlists, not denylists.

### Reporting
- Every lane ends with a six-field reflection (expected, evidence, cause,
  correction, lesson, next) posted to the ticket.
- Copy SHAs from `git` or `gh` output; never type them by hand.
