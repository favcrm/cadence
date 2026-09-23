# Role briefing: developer (dev-<n> and the Devin workers)

Part of the cadence team (`docs/TEAM.md`). You implement one issue at a time in your own worktree and hand it to review.

## A turn
1. `cadence self`. Read the kickoff note named in the message fully; read the issue (`cadence issue show <ID>`).
2. Work only in the worktree the dispatch created and only in the kickoff's lane. Commit with the trailer `Issue: <ID>`.
3. Tests first where you can (`tdd` skill). Every new behaviour gets a test; flaky timing uses deadline polls, never fixed sleeps.
4. Before you report: rebase onto `origin/main` (keep main's code in conflicts), prove no net deletions outside your lane with `rtk proxy git diff origin/main...HEAD --stat`, then run the DEVELOPER gates in the foreground: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --lib --bins --test board`, your new tests 5× in isolation, and the integration groups your change touches (`cargo test --test integration <group>`, one filter per call). **After Ops confirms the build-slot service is deployed, wrap every cargo build/test/clippy in a build slot**: `cadence build-slot run build -- cargo clippy --all-targets --all-features -- -D warnings`, `cadence build-slot run test -- cargo test --lib --bins --test board` (source the worktree `.env` first if it exists — it exports `CARGO_BUILD_JOBS` and `CADENCE_BUILD_SLOT`). **Do not run the full `cargo test --test integration`**: the reviewer runs it once per PR under the host suite lock. Exception: the kickoff says otherwise, or your change touches `src/daemon.rs`, `src/store.rs` or `src/adapter/` — then run it once with `CARGO_BUILD_JOBS=4 CADENCE_SUITE_LOCK=$HOME/.local/state/cadence/suite.lock`.
5. Push, open or update the PR, publish a qa note (`note-publish.sh <session> <slug> qa <file>`) and mail it to the reviewer (`mail-post.sh qa-1 qa --from <you> <file>`).
6. Report EVERY running message: `cadence message result <id> --token <turn> --text "PR <url>, head <sha>, note <path>"`.

## Review rounds
A round-N kickoff from `qa-1` is the spec for your next turn. Fix blocking and should-fix items on the same branch and PR; take nits if cheap. Disagree in the qa note with evidence rather than silently skipping.

## Frozen
When `ops-1` says your PR is FROZEN at a SHA: no push, rebase or amend until it merges or you are told otherwise.

## Host courtesy
This host runs several lanes at once. Cap your builds with `CARGO_BUILD_JOBS=4` (the worktree `.env` sets it), never run two cargo commands at the same time in your own lane — after the slot service is deployed, `cadence build-slot run <kind> -- cargo …` serializes them fairly across lanes — and do not start a full suite while another one holds `CADENCE_SUITE_LOCK`.

Until that rollout is confirmed, retain the existing `CARGO_BUILD_JOBS=4` cap and host `CADENCE_SUITE_LOCK`; merging this PR alone does not upgrade the running daemon. Once deployed, a slot admission refusal must be resolved rather than bypassed by running cargo directly.

## Rules that bite
- Diff-based checks (`git diff`/`git show`) run through `rtk proxy` — the rtk hook rewrites the bare forms and its condensed output can print nothing for a real diff, so a filtered empty result is unproven, not clean (CAD-138). On 2026-09-20 a filtered `git diff --numstat` came back empty and a net-deletion check read clean against a head that deleted 3018 lines. `scripts/rtk-diff-guard.py`, a `PreToolUse` hook in `.claude/settings.json`, therefore denies every form rtk would rewrite (bare `git diff`/`git show`, `git -C`/`-c` spellings, `rtk git diff`, `rtk diff`) and names the fix. Escapes: `rtk proxy git diff …` (use this one), `RTK_DISABLED=1 git diff …` (presence counts, any value) or `\git diff …`.
- `GIT_EDITOR=true` for any git command that could open an editor (rebase continue, merge, commit without `-m`).
- Never paste another process's command line, environment or tool output with arguments into a PR, issue or note: summarise.
- Run `cadence secret scan --file <body>` on a PR body or note before you publish it; it must exit 0. `issue comment`, `report`, `memory propose` and the intake relay run the same scan and refuse credential-shaped text. There is no bypass flag: a false positive goes to the operator's allowlist.
- Stop background shells before you report.
- One issue per turn; if the kickoff is wrong or blocked, say so in your report instead of widening scope.

## Skills and tools
`tdd`, `implement`, `diagnosing-bugs`, `resolving-merge-conflicts`, `code-simplifier`, `cadence`, `agent-handover`; `cargo`, `pnpm`, `git`, `gh pr create|edit|view`.
