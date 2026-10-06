# Managed checkout lifecycle (CAD-848)

Cadence-created development, review and validation checkouts use the repo's
`.cadence/` ledger at `.cadence/managed-checkouts.json`. Issue refs bind a
development lane to its ticket. Cleanup also checks the project-declared repo,
managed layout and canonical Git worktree registration. If lifecycle metadata is
present, it must match the active development issue/branch and moves to
`releasing` before checkout deletion. The ledger lock stays held through
checkout deletion and the final state write: a retain that wins first blocks
release, while one that races a completed release cannot rewrite it. Idle target
cache reclaim does not change lifecycle state; it holds the same lock across its
final ownership/process revalidation and deletion, serializing retain/adopt.
Legacy issue refs remain authoritative.

## Create and recover

- `cadence issue start <ID>` fetches the `origin/HEAD` branch when available,
  pins that commit SHA, records a `preparing` entry, creates the lane, then
  applies the existing cargo target, pre-push hook and slot environment.
  Successful setup becomes `active` and the issue's branch/worktree refs stay
  the durable issue binding.
- A same-issue restart reuses only its recorded repo/path/branch and does not
  reset dirty source. A different repo or checked-out branch refuses. A
  `retained` or `releasing` record is never implicitly reactivated. A `released`
  record starts a new generation only on an explicit start/review request when
  its path is absent and unregistered; development starts also require matching
  recorded branch/worktree refs. For an existing released checkout, use explicit
  `checkout resume`; it verifies the exact registered path, branch/detached state
  and pinned HEAD without resetting or touching tracker refs. A retained checkout
  must first be explicitly released. Setup failures preserve the checkout and
  branch and are recorded as `setup-failed`; retry `issue start` after correcting
  the cause. `preparing`
  after interruption means inspect the path and branch, then retry to heal; it
  never authorizes removal.
- `cadence review` records its tool, owner and exact PR SHA before creating a
  detached tree. Normal completion records `released`; `--keep` records
  `retained`. Clean merge-result restoration, receipt writes, final safety
  checks and removal share one release lock; failures retain the checkout with
  a reason. A merge conflict is never auto-aborted, checked out or reset: the
  conflicted checkout, index and `MERGE_HEAD` are retained for explicit owner
  inspection, no gates run on that tree, and the blocked result names the lane
  and conflict paths. Comparison base checkouts use their own release guard
  before removal; `--keep` or a guard/safety refusal retains them. The
  Markdown/JSON receipts are declared outside the disposable tree. Validation
  tools should use the same registration/release interface.

## Inventory and explicit adoption

```sh
cadence issue checkout inventory --repo <repo>
cadence issue checkout adopt --repo <repo> --path <path> \
  --purpose validation --tool <tool> --owner <owner> \
  --pinned-sha <full-40-hex-sha> [--branch <branch>] \
  [--release-artifact <path>] [--rollback-artifact <path>]
cadence issue checkout release --repo <repo> --path <path> --reason "<reason>"
cadence issue checkout resume --repo <repo> --path <path> \
  --pinned-sha <full-40-hex-sha> --reason "<reason>"
cadence issue checkout retain --repo <repo> --path <path> --reason "<reason>"
```

Inventory emits `cadence.worktree-inventory/1` JSON. It reports managed state,
missing/moved paths, dirty and dirty-merged checkouts, unknown/unmanaged trees,
retained artifacts, byte estimates and reason codes. A `null` reclaim estimate
means unknown. Inventory does not modify Git, the tracker or the ledger, and
sends no notifications. Unregistered existing trees—including trees outside
`.cadence/wt`—remain inventory-only. Adoption requires explicit repo, path,
purpose, tool, owner and exact current SHA; it validates the path, Git common
dir, HEAD, branch and registration while holding the ledger lock rather than
trusting path names or timestamps. Development lanes remain
bound by their issue refs and must be in a project-declared repo and registered
at the canonical managed path before finish can remove them.

Resume accepts only one valid `released` record whose exact canonical checkout
still exists as a registered linked worktree of the recorded repo. The supplied
full SHA must equal current HEAD; the registered branch/detached state must match
the record. Non-development checkouts must still match their recorded pin;
development may have advanced on its same recorded branch, in which case the
pin is updated to its current HEAD. Owner, tool, purpose, issue binding and
recovery artifact declarations are preserved. No Git checkout/reset or tracker
write occurs. `checkout retain` applies to active checkouts only.

To recover a retained checkout, explicitly release it with a reason, then resume
it at its verified current SHA. If inventory shows `releasing`, inspect the
checkout and use `checkout release --reason` to record an explicit recovery;
the ledger lock serializes this against any live release guard. Then resume using
the exact current SHA. A failed path, branch, pin or registration check leaves
the record unchanged. Never edit the ledger or force a checkout active.

## Cleanup policy

`cadence issue reclaim [--idle-secs N]` is a read-only plan. It combines the
merged-lane finish plan and idle `target/` cache plan using the same checks as
the scheduled daemon pass. A retained, releasing or otherwise non-active
lifecycle record is report-only for cache cleanup; the plan names its state and
reason. Legacy lanes require the declared repo, canonical real worktree,
registered matching branch and open issue refs. `cadence issue reclaim --apply`
opts into the existing policy: merged checkout finish and bounded lane-local
target cache reclamation. Immediately before the final process scan, reclaim
holds the lifecycle lock through ownership revalidation and deletion; it drops
the guard before writing a PM comment. Cache reclamation never changes lifecycle
state or removes source or a branch. Branch deletion requires the issue-bound, project-declared
checkout and exact-tip merge/push evidence; any present lifecycle record must
also match and be active. Squash merges use the existing SHA-pinned ancestry,
patch-equivalence or merged-PR evidence. Unmanaged or foreign checkouts remain
inventory-only until explicitly adopted.

New review/validation checkout deletion has **no automatic cleanup policy**;
inventory is report-only. Age, name prefix, `/tmp` location, ticket status and
ancestry alone authorize nothing. Released tools must keep receipts and any
listed release/rollback artifacts outside disposable outputs. Uncertain owner,
failed process/agent/task enumeration, open cwd/FD use, dirty state, path or
symlink mismatch, a moved branch, a surviving unmerged tip or stale evidence
retains the resource with an actionable reason. `--force` cannot override
failed process/daemon binding, dirty-tree or activity enumeration; uncertainty
is not a clean result.

Process scans first prove that the canonical proc root has one full procfs
mount (`mountinfo` root `/`) with no restricted `hidepid` mode; absent,
unreadable, malformed, stacked or restricted visibility metadata refuses the
scan. They then inspect cwd and open-FD holders without using UID, group,
capability or path-permission heuristics to infer that a process lacks an
inherited or transferred checkout descriptor. A complete status with `State: Z`
or `State: X` is the only basis for skipping a dead process; missing or
malformed state and inaccessible live status, cwd or FD inspection remain
incomplete enumeration. Any such failure retains the resource with the process
identity and refusal reason when known. This deliberately fails closed when the
host cannot prove a complete process view; cleanup requires an authorized
complete inspection mechanism rather than suppressing unrelated `EACCES`.

A successful review using a clean no-commit merge-result tree declares its
receipt paths, then acquires an exact-owner release guard before verifying and
restoring the tree to the detached pinned PR head. The guard remains held while
report receipts are written, the final safety scan runs and the checkout is
removed. If restoration, receipt writing or any safety check fails, the review
is blocked and the checkout is retained with a recovery reason; it is never
force-reset.

For an absent checkout, `issue finish <ID> --worktree <path>` closes the
worktree ref without deleting Git state. The final missing-path and moved-branch
checks run under the lifecycle lock before an active record enters `releasing`;
retained, interrupted `releasing` and other non-active records refuse before
tracker or branch mutation. A refs-only finish never removes a directory, even
if the recorded path reappears after its initial probe. An already `released`
record is locked but not rewritten, preserving its explicit historical reason.
Generic state transitions cannot resolve `releasing`; inspect the checkout and
use `checkout release --reason` as the explicit recovery, then resume only at a
verified current SHA. A surviving branch ref remains open and its commits
remain recoverable. If both checkout and branch refs are missing, the tracker
refs may be closed as history. A branch checked out at a different path is
reported as moved and is never removed from under its new checkout.

## Report-only rollout and rollback

Ship with new tool-checkout removal disabled: inventory and explicit lifecycle
release/retention are available, but no janitor removes released tool trees.
Operators can compare reports and verify ownership/artifact declarations
before a separately approved policy enables any new deletion scope. Rollback
is source-level: stop invoking explicit release/adoption commands, retain the
ledger for recovery, and revert the code change; no production rollout,
tracker migration or artifact deletion is required. Existing scheduled
merged-lane and idle-target policy remains unchanged.
