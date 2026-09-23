# Cadence proposed-memory audit — reviewer A

Date: 2026-09-21. This is a read-only content audit. The inspected product
reference is `origin/main` at `b59e0384cbd248f642b8b3e3cd69ebf839847e34`.
All eleven files were `status: proposed`; there were no accepted records to
approve, reject, or rewrite.

Each row is this reviewer’s independent result (`1/2`): one independent
review is recorded here, and no row is claimed as the required `2/2` quorum.
The source implementation does not currently enforce an authenticated
independent-reviewer/`accepted_by` field or two-reviewer quorum. `src/memory`
has a curator role gate and `verified_at`, while the development-team design
still describes record-level evidence, reviewer identity, and invalidation as
implementation work. These content results therefore do not change memory
status.

## Results

| file (SHA-256 of exact inspected bytes) | result | evidence and limit |
|---|---|---|
| `a-plan-that-cites-bare-risk-clas.md` — `6b92794baf6fe9df641fae043669f1c738089b6864ab1a6bc2c3349344d4b612` | **needs-revision, 1/2** | The reusable rule to cite the dated policy, quoted clause, and commit is sound. Current `docs/roles/risk-classes.md` at `c183a8d` still has only `human` and `auto`; the alleged `notify` class/same-day rewrite premise is unverified and absent from the current tree. Narrow the stale-label example and remove the unqualified “cannot be re-checked” claim. This is guidance, not an enforced memory gate. |
| `an-acceptance-check-of-the-shape.md` — `01547a74b3be79059069969584ab405b1799bcad69fa4acd1ce5f7bde3702ef9` | **needs-revision, 1/2** | The useful observation is supported: no-match `grep -c` prints `0`, while `test -n "0"` succeeds, so a non-empty counter is not a positive-count receipt. The stored wording is wrong about status: in Bash, `n=$(grep -c ...)` returns the substitution’s status (`1` here); command substitution does not itself always discard it, and `set -e` can stop at the assignment. Rewrite the lesson around numeric assertions or direct `grep` status, and avoid “always passes”/“discards the exit status” as general claims. |
| `browser-agents-cannot-reach-loop.md` — `78ba64308c880f800f49a099922e52416918c1ad4ea3382e2cbe650984749309` | **needs-revision, 1/2** | The tested default/remote browser route failed for the recorded loopback cases, but the absolute claim is contradicted by a current local route: `AGENT_BROWSER_FORCE_LOCAL=1` rendered and clicked the live UI on `127.0.0.1:3010`. State provider, host, URL, and date; distinguish remote-wrapper failure from loopback reachability and add expiry conditions. |
| `delivery-retry-budget-survives-gate-waits.md` — `2465caa88ffd9819c114fc22bd60dbc664440bef5c2a224b2dd7762ab761f471` | **needs-revision, 1/2** | The historical gate-wait retry-loss diagnosis is supported. Current `origin/main` contains the bounded routed-PTY fix (`2d5a6a3`) and regression preserving four attempts across a busy gate, so the file’s “pending independent validation/implementation” wording is stale. Bind the lesson to the fix/test SHA and retain the retry-zero and four-attempt limits. Current enforcement still does not provide a memory two-review quorum. |
| `empty-git-diff-output-on-this-ho.md` — `d5b83da3c7805fa1ffc36f6d0899f8748c12d0fdf7de0290c3b2015d308865b1` | **needs-revision, 1/2** | The filtered `rtk`/diff observation has reproductions and is actionable for the recorded tool/version/path. The final claim that `docs/roles/dev.md` still prescribes bare `git diff` is false at current `origin/main`; it prescribes `rtk proxy`. Scope the lesson to the affected wrapper/version and mention the current guard/mitigation rather than generalizing to Git. |
| `env-target-dir-can-break-fixture-paths.md` — `856f24d49694b473dcfc1db5eeef6a3692d72f2aaa0c086c315d8659950a787c` | **needs-revision, 1/2** | PR70 evidence supports inherited `CARGO_TARGET_DIR` causing fixture-path failures and explicit CLI target-dir/unset-env correcting the invocation. Bind the claim to the corrected PR/head and preserve the important condition: clearing the variable is appropriate only when the test is not intentionally testing env precedence; current worktree code intentionally gives that variable precedence. |
| `filtered-tests-need-exact-nonempty-receipts.md` — `1057b99feb87d1149a0c3425bee50ae81df06bb57244ae75110cbdb6b54b1b77` | **supported, 1/2** | The CAD225/PR101 harness and current scripts require exact filters, nonempty selection, dirty-tree refusal, explicit suite locking, unchanged head/tree, and zero retries. Keep the scope to acceptance runners and inventory-backed test execution; it does not establish independent lesson-promotion support, which the harness still lists as unsupported. |
| `qa-verdicts-expire-on-head-change.md` — `e4f2dd1c1ad81b2220d4bcd1cef95acbe08a5663d33e3caa8aeca5e5d295438f` | **supported, 1/2** | Current `scripts/qa-verdict.sh` resolves the current head and rejects a stale `--sha`; `docs/SESSION.md` says a later push moves the head and the verdict does not follow. This binds QA evidence to a commit, but does not itself prove independent reviewer identity or a two-person memory gate. |
| `queued-delivery-is-not-execution.md` — `2b116adf863a981f9d62cfdca749f9e7af8c74fcd2848bda8afd4c493c7c6339` | **supported, 1/2** | Current protocol and daemon/store state distinguish queued, submitting, running, completed, failed, and unknown; inbox receipt is not provider execution. The lesson should retain that state distinction and must not claim that a queue receipt proves execution completion. |
| `reading-a-pr-with-git-checkout-r.md` — `e364b90b93f9b73cf396729847aec40efee1560c3a090ef41102cdb4d240031c` | **supported, 1/2** | The CAD187 scratch reproduction shows `git checkout <ref> -- .` changes the index/worktree while leaving HEAD on the current branch; `git show <ref>:path` is the clean inspection control. This is a procedural Git fact, not an assertion that every checkout command is unsafe. |
| `shared-structs-need-all-target-validation.md` — `2593b29d3fe77c142ace8b5d8506b3b6f0f5f461daa126b1d1d31406de9f976d` | **supported, 1/2** | PR73’s missing `Front.kind` initializer was caught outside the focused path and fixed by all-target validation; current CI includes all-target nextest, doctests, and all-target clippy. Keep the scope to shared Rust model/serde changes rather than claiming every source change needs the same breadth. |

## Boundary

The current product can require a curator in the relevant Cadence pane, but
the source does not yet record two authenticated independent reviewers,
content hashes, evidence references, or an atomic quorum transition for these
memories. The rows above are therefore evidence for a later curator decision,
not an acceptance action. No file in the memory directory, tracker, runtime,
or public note was changed by this audit.
