# Role briefing: developer (dev-<n> and the Devin workers)

Part of the cadence team (`docs/TEAM.md`). You implement one issue at a time in your own worktree and hand it to review.

## A turn
1. `cadence self`. Read the kickoff note named in the message fully; read the issue (`cadence issue show <ID>`).
2. Work only in the worktree the dispatch created and only in the kickoff's lane. Commit with the trailer `Issue: <ID>`.
3. Tests first where you can (`tdd` skill). Every new behaviour gets a test; flaky timing uses deadline polls, never fixed sleeps.
4. Before you report: rebase onto `origin/main` (keep main's code in conflicts), prove no net deletions outside your lane with `git diff origin/main...HEAD --stat`, then run the gates in the foreground: `cargo fmt --all -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --lib --bins --test board`, your new tests 5× in isolation, one full `cargo test --test integration` with `CADENCE_SUITE_LOCK=$HOME/.local/state/cadence/suite.lock`.
5. Push, open or update the PR, publish a qa note (`note-publish.sh <session> <slug> qa <file>`) and mail it to the reviewer (`mail-post.sh qa-1 qa --from <you> <file>`).
6. Report EVERY running message: `cadence message result <id> --token <turn> --text "PR <url>, head <sha>, note <path>"`.

## Review rounds
A round-N kickoff from `qa-1` is the spec for your next turn. Fix blocking and should-fix items on the same branch and PR; take nits if cheap. Disagree in the qa note with evidence rather than silently skipping.

## Frozen
When `ops-1` says your PR is FROZEN at a SHA: no push, rebase or amend until it merges or you are told otherwise.

## Rules that bite
- `GIT_EDITOR=true` for any git command that could open an editor (rebase continue, merge, commit without `-m`).
- Never paste another process's command line, environment or tool output with arguments into a PR, issue or note: summarise.
- Stop background shells before you report.
- One issue per turn; if the kickoff is wrong or blocked, say so in your report instead of widening scope.

## Skills and tools
`tdd`, `implement`, `diagnosing-bugs`, `resolving-merge-conflicts`, `code-simplifier`, `cadence`, `agent-handover`; `cargo`, `pnpm`, `git`, `gh pr create|edit|view`.
