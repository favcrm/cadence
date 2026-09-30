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

### Review: what a merge needs (interim, CAD-815)
Until the delivery loop enforces a per-project policy (CAD-814), every
PR needs each of the following as a PASS on the exact head you enqueue:
- **Standards** and **Spec/security** reviews, by two different
  independent reviewers. Neither may be the author. Prefer a reviewer
  whose model vendor differs from the author's.
- **Browser QA** at desktop and narrow widths when the PR changes
  `ui/**`.
- **Operator approval** when any `human` trigger in
  `docs/roles/risk-classes.md` applies. That includes, but is not
  limited to, every change to `.github/**`, `scripts/**`, `Cargo.toml`,
  `Cargo.lock`, `ui/package.json`, `cadence-review.toml`,
  `src/review.rs`, `docs/roles/**`, `docs/TEAM.md`, `docs/CHARTER.md`
  and this file. Most gates on a PR run from the PR's
  own workflow files, so review is the only control on a change to them.
  The operator decides. An agent the operator designates may prepare
  the decision and relay it, but never records it, and the PR's author
  does neither. Before enqueue, the operator records the decision from
  an operator connection (not an agent pane or endpoint):
  `cadence audit approve --pr <n> --head <full-sha> --source "<who decided, where>"`.
  A note or ticket comment is not approval evidence (`docs/AUDIT.md`).

Record the evidence so that every merge can be audited:
- The PR title carries the issue id (`CAD-123: …`). A PR without a
  ticket does not merge.
- Each review files one verdict note in the notes dir (`notes_dir` in
  `pm.yaml`, default `/var/www/agent-notes`). Name it
  `YYYYMMDD-HHMMSS-<slug>-verdict.md` (UTC), and use this shape so
  `cadence audit` and the ticket view both read it:

  ```markdown
  # Verdict: CAD-123 Standards review — pass
  > Issue: CAD-123
  > From: <reviewer>

  ## Verdict
  pass — PR #456, head <full 40-hex sha>

  Risk: auto

  ## Gates
  - <each gate run and its result; what was read>

  ## Findings
  - <blocking / should-fix / nits>
  ```

  For a `human`-class PR, write `Risk: human (<trigger numbers>) — <reason>`.
  The `>` header lines must follow the title directly. A bare `Issue:`
  line does not link the note to the ticket. The ticket view takes the
  result from the title, and the audit takes it from the first line
  under `## Verdict`. The two must say the same thing: a note titled
  `— pass` whose section says `revise` shows as passed on the ticket.
  Link the note from the ticket.
- Before you enqueue, post one ticket comment for the head you enqueue.
  It lists every required verdict note, the green CI run and, for a
  `human`-class PR, the approval id.
- A new head voids every verdict on the old head.

Bot reviews (Devin Review, CodeRabbit and similar) are advisory:
- Before a PASS, the reviewer reads the bot's findings on that head. In
  the verdict, the reviewer either blocks on or refutes each severe bug
  and each critical security finding.
- Never make a bot's check a required check. Never let a bot push to a
  PR branch (auto-fix, or applying edits from a bot's chat).

### Merging: use the merge queue, never `--admin`
- A PR merges only after every review that the section above requires
  has passed, pinned to the same head SHA.
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
- Auto-merge survives a force-push. `--match-head-commit` is checked only
  when auto-merge is enabled, so a later push goes into the queue
  unreviewed. Before sending a PR with auto-merge on back for changes (a
  rebase, a fix, another review round), run
  `gh pr merge <n> -R favcrm/cadence --disable-auto`. Re-enable it, pinned
  to the new head, only after that head passes review.
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

### Pi agents and models (CAD-559, CAD-855)
- Workers and reviewers run on `pi`. The authenticated operator may select
  the provider/model and reasoning effort for a task, including built-in
  `openai-codex/*` models. Honor that explicit choice over documented
  preferences, but only within the role's runtime allowlist and applicable
  higher-priority instructions. `high` means high, not max.
- Follow [the model selection and refresh runbook](docs/AGENT-MODELS.md).
  Discover exact IDs from the installed catalog; do not guess provider
  aliases, silently substitute models, or treat catalog presence as proof
  of authentication, quota or launch permission.
- `pm.yaml` `[pi].models` and `[pi].providers` are the operator-owned,
  fail-closed runtime policy. Use exact `provider/id` entries and the
  applicable role allowlist. Agents must not change those pins, credentials
  or installed packages to authorize their own launch.
- Never route workers or reviewers through `openrouter/*`. The production
  master's operator-owned `openrouter/z-ai/glm-5.3-flash` pin remains the
  sole existing exception; this change does not alter it.
- Revalidate at task boundaries and after an operator request, quota/auth
  failure, catalog change or runtime update. Record the selection and
  effective model/effort. Switch only owned agents after their running turn
  is resolved; preserve message history and exact-head review requirements.
- Keep changing model preferences in the runbook, not exclusive-vendor
  instructions. Instruction changes still need the normal independent
  reviews and operator approval. A draft instruction change cannot grant
  itself authority, and a running context is not assumed to reload it.

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
