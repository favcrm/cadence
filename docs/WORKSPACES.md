# Development workspaces

Use one repository directory with task-named Git worktrees inside it:

```text
cadence/
  .git/
  src/
  .worktrees/
    codex-terminal/
    review-m2a/
```

The root checkout is the PM workspace. Each implementation task gets its own
branch and directory; a reviewer can use a detached checkout at the exact commit.
These directories share Git history, but have independent working files and
indexes. Name them for tasks, not providers, so another agent can take over.
`/.worktrees/` is ignored; never add a nested checkout to a commit.

From the repository root, create an implementation checkout with:

```bash
git worktree add .worktrees/my-task -b feat/my-task main
```

Create a review checkout by replacing `COMMIT_SHA` with the reported revision:

```bash
git worktree add --detach .worktrees/review-my-task COMMIT_SHA
```

Every kickoff states the absolute task directory, branch, reviewed base,
exclusive ownership and working return route. Run commands with that directory
explicitly. Do not switch branches or edit files in another agent's checkout.
Worktree paths do not identify provider sessions or message endpoints.

## Relocation and cleanup

Pause the owner and finish active tests before moving a checkout. Use
`git worktree move OLD_PATH .worktrees/TASK`, not a filesystem rename. Update
current kickoff instructions, registered working directories and launch commands;
do not edit historical QA notes to imply they ran somewhere else. Restart only
owned task processes that need a new working directory.

Before removing a completed worktree, check its status and preserve all unmerged
commits and useful untracked evidence. Run `git worktree remove PATH` without
`--force`; unresolved files should stop cleanup. Remove branches only after their
work has merged or been intentionally superseded. Use `cargo clean` for redundant
Rust build artifacts after tests finish, rather than deleting arbitrary files.

Temporary state, credentials, transcripts and machine-specific evidence stay
outside version control. Keep a live communication bridge until its replacement
passes end-to-end testing. Cleaning a workspace is not permission to stop user
terminals or delete agent session history.
