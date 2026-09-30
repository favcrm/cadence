# Merge risk classes (shared by qa-1 and ops-1) — operator decision 2026-09-19

Goal: autonomous delivery. Routine PRs merge without a human; only substantial changes wait for the operator.

## Class `human` — the operator approves (any ONE trigger is enough)
1. **Trust boundary:** turn tokens, generations, session binding or ownership proofs, adoption/recovery (`recover()`, `open_adopted`, `verify_ownership`, `resolve_session`), the approval broker, permission modes, bypass handling, auth or identity (`request_identity`, `write_caller`, `tailnet_proof`, tailnet).
2. **Data and deletion:** schema migrations or any change to the store version; code that deletes or rewrites user data (worktree/branch removal, `issue finish`, `agent gc`, tracker write path, memory writes); anything that can lose commits.
3. **Security:** fixes for a leak or vulnerability, secret handling, redaction logic.
4. **Supply chain and CI:** new or upgraded dependencies (`Cargo.toml`, `Cargo.lock`, `ui/package.json`), `.github/workflows/*`, scripts that post statuses.
5. **Size or contention:** more than 1500 changed lines (excluding fixtures and lockfile churn), a third review round, or an unresolved reviewer–author disagreement.
6. **Outward or fleet-wide actions** (see also `docs/CHARTER.md` non-goals): releases, tags, repo settings, anything posted publicly other than the PR merge itself; a `daemon restart` that is not `--when-idle`; killing processes cadence did not start; any `--force`.

7. **The rules and the gates themselves:** `docs/roles/*`, `docs/TEAM.md`, `docs/CHARTER.md`, `cadence-review.toml`, `src/review.rs`, `scripts/*`, and any change to who approves what, to a gate, or to this file. A PR that rewrites the rules can never approve itself.

## Class `auto` — ops-1 merges on its own
Everything else, provided ALL hold: qa-1 verdict `pass` on the exact head SHA; CI green on that head; the combined-tree gate (train) green including one full integration suite under the suite lock; net-deletion check clean; the author is frozen; no secret-looking string in the PR body or diff.

## Who decides
qa-1 states `Risk: auto` or `Risk: human (<trigger numbers>)` in every verdict with one line of reasons. ops-1 re-checks mechanically (paths touched, `Cargo.toml`/workflow diffs, line count, round count). If either says `human`, it is `human`. When unsure, `human`.

### Reviewer count on a solo-operator lane
The default is two distinct independent reviewers (Standards and
Spec/security must come from different people). On a project where the
author is the only registered reviewer-capable identity — a solo-operator
lane — a **single** independent reviewer (never the author) may cover both
axes, but only when the operator approves that head through
`cadence audit approve --pr <n> --head <full sha> --action merge`. That
verb is operator-connection-only and writes an `approval_recorded` event
on the dedicated `audit:approvals` stream bound to the head — a ticket
comment or note is not approval evidence and never satisfies this clause.
The operator approval stands in for the *second reviewer*, not for
independence. Whether a project is a solo-operator lane is declared in its
`delivery:` policy (which identities are registered reviewers), not ad-hoc
per PR; the clause does not relax any other `human` trigger, and it never
lowers Browser QA, the `qa-verdict` status binding, or the risk class
itself. A change to *this* rule is class `human` (trigger 7) and can
never approve itself.

## After an auto merge
ops-1 runs the post-merge list, then a smoke check on the live system (`cadence --version`, `cadence status`, board 200, `cadence doctor`), and sends fable-cc one line: `merged #N <sha> (auto): <title>; tree ok; smoke ok`. If the smoke check fails: open a revert PR immediately (`gh pr create` with `git revert`), mark it `human`, and escalate to fable-cc. Do not merge the revert without approval.
