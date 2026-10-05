# Managed checkout lifecycle (CAD-848)

Cadence-created development, review and validation checkouts use the repo's
`.cadence/` ledger at `.cadence/managed-checkouts.json`. Issue refs remain the
authority for development lanes; the ledger records setup/release state and
tool ownership. It is not a deletion permit.

## Create and recover

- `cadence issue start <ID>` fetches the `origin/HEAD` branch when available,
  pins that commit SHA, records a `preparing` entry, creates the lane, then
  applies the existing cargo target, pre-push hook and slot environment.
  Successful setup becomes `active` and the issue's branch/worktree refs stay
  the durable issue binding.
- A same-issue restart reuses only its recorded repo/path/branch and does not
  reset dirty source. A different repo or checked-out branch refuses. Setup
  failures are recorded as `setup-failed`; retry `issue start` after correcting
  the cause. `preparing` after interruption means inspect the path and branch,
  then retry to heal; it never authorizes removal.
- `cadence review` records its tool, owner and exact PR SHA before creating a
  detached tree. Normal completion records `released`; `--keep` records
  `retained`. Its Markdown/JSON receipts are declared outside the disposable
  tree. Validation tools should use the same registration/release interface.

## Inventory and explicit adoption

```sh
cadence issue checkout inventory --repo <repo>
cadence issue checkout adopt --repo <repo> --path <path> \
  --purpose validation --tool <tool> --owner <owner> \
  --pinned-sha <full-40-hex-sha> [--branch <branch>] \
  [--release-artifact <path>] [--rollback-artifact <path>]
cadence issue checkout release --repo <repo> --path <path> --reason "<reason>"
cadence issue checkout retain --repo <repo> --path <path> --reason "<reason>"
```

Inventory emits `cadence.worktree-inventory/1` JSON. It reports managed state,
missing/moved paths, dirty and dirty-merged checkouts, unknown/unmanaged trees,
retained artifacts, byte estimates and reason codes. A `null` reclaim estimate
means unknown. Inventory does not modify Git, the tracker or the ledger, and
sends no notifications. Unregistered existing trees—including trees outside
`.cadence/wt`—remain inventory-only. Adoption requires explicit repo, path,
purpose, tool, owner and exact current SHA; it verifies the Git common dir and
branch rather than trusting path names or timestamps.

## Cleanup policy

`cadence issue reclaim [--idle-secs N]` is a read-only plan. It combines the
merged-lane finish plan and idle `target/` cache plan using the same checks as
the scheduled daemon pass. `cadence issue reclaim --apply` opts into the
existing policy: merged checkout finish and bounded lane-local target cache
reclamation. It revalidates before deletion. Cache reclamation never removes
source or a branch. Exact-tip merge evidence remains the only authorization to
delete a branch; squash merges use the existing SHA-pinned ancestry,
patch-equivalence or merged-PR evidence.

New review/validation checkout deletion has **no automatic cleanup policy**;
inventory is report-only. Age, name prefix, `/tmp` location, ticket status and
ancestry alone authorize nothing. Released tools must keep receipts and any
listed release/rollback artifacts outside disposable outputs. Uncertain owner,
failed process/agent/task enumeration, open cwd/FD use, dirty state, path or
symlink mismatch, a moved branch, a surviving unmerged tip or stale evidence
retains the resource with an actionable reason. `--force` cannot override
failed process/daemon binding, dirty-tree or activity enumeration; uncertainty
is not a clean result.

For an absent checkout, `issue finish <ID> --worktree <path>` closes the
worktree ref without deleting Git state. A surviving branch ref remains open
and its commits remain recoverable. If both checkout and branch refs are
missing, the tracker refs may be closed as history. A branch checked out at a
different path is reported as moved and is never removed from under its new
checkout.

## Report-only rollout and rollback

Ship with new tool-checkout removal disabled: inventory and explicit lifecycle
release/retention are available, but no janitor removes released tool trees.
Operators can compare reports and verify ownership/artifact declarations
before a separately approved policy enables any new deletion scope. Rollback
is source-level: stop invoking explicit release/adoption commands, retain the
ledger for recovery, and revert the code change; no production rollout,
tracker migration or artifact deletion is required. Existing scheduled
merged-lane and idle-target policy remains unchanged.
