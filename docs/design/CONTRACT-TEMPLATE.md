# Design contract: <ID> <rule being enforced>

Optional long form of the design note (AGENTS.md "Design note in the PR",
CAD-1099). Put it in the PR description, or on the ticket when the operator
asks for agreement before code. Code reviewers check the code against it. Delete none of the headings; write "none, because ..." where a section is empty.

## Invariants
What must always hold, as testable sentences. Name the actor allowed to act,
the state it acts on, and who or what is refused.
- I1: <e.g. only an operator connection can record X; an agent pane or endpoint cannot>
- I2: <e.g. X is applied exactly once per (pr, head)>

## Failure modes
Mark each: the invariant it threatens (I#), how the guard holds, or "n/a" with a
reason. A bare "n/a" fails review.
- [ ] Crash or SIGKILL between any two steps (leaves what on disk? does recovery re-run or skip?)
- [ ] Loaded host (slow step, timeout, retry: can a late or duplicate action land?)
- [ ] Wrong caller (agent pane, endpoint or detached child instead of the permitted actor)
- [ ] Concurrent callers (same request twice, two different requests racing)
- [ ] Forked, detached or `setsid` child (outlives the caller, inherits the fd, token or lock)
- [ ] Relay paths (a board or HTTP peer at least as strict as the daemon RPC it relays)
- [ ] Forged field (caller-supplied id, actor, head, token, path or timestamp)
- [ ] Partial write (torn file, half-applied multi-record update, event without its effect)
- [ ] Clock or TTL edges (expiry at the boundary, clock step, restart mid-lease)

## Adversarial tests
One row per test. Each is written first and must fail without the guard.
Name the test, the invariant, the guard it proves, and how it fails without
the guard. "Would pass anyway" means the test is wrong.

| Test name | Proves (I#) | Guard | Fails without the guard because |
|---|---|---|---|
| `<test_fn_name>` | I1 | `<fn or check>` | `<observable wrong outcome>` |

## Out of scope
What this change does not enforce, and where that is tracked (ticket id).
