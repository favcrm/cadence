# Merge risk classes (shared by qa-1 and ops-1) — operator decision 2026-09-19

Goal: autonomous delivery. Routine PRs merge without a human; only substantial changes wait for the operator.

## Class `human` — the operator approves (any ONE trigger is enough)
1. **Trust boundary:** turn tokens, generations, session binding or ownership proofs, adoption/recovery (`recover()`, `open_adopted`, `verify_ownership`, `resolve_session`), the approval broker, permission modes, bypass handling, auth or identity (`request_identity`, `write_caller`, `tailnet_proof`, tailnet).
2. **Data and deletion:** schema migrations or any change to the store version; code that deletes or rewrites user data (worktree/branch removal, `issue finish`, `agent gc`, tracker write path, memory writes); anything that can lose commits.
3. **Security:** fixes for a leak or vulnerability, secret handling, redaction logic.
4. **Supply chain and CI:** new or upgraded dependencies (`Cargo.toml`, `Cargo.lock`, `ui/package.json`), `.github/workflows/*`, scripts that post statuses.
5. **Size or contention:** more than 1500 changed lines (excluding fixtures and lockfile churn), a third review round, or an unresolved reviewer–author disagreement.
6. **Outward or fleet-wide actions** (see also `docs/CHARTER.md` non-goals): releases, tags, repo settings, anything posted publicly other than the PR merge itself; a `daemon restart` that is not `--when-idle`; killing processes cadence did not start; any `--force`.

7. **The rules and the gates themselves:** `docs/roles/*`, `docs/TEAM.md`, `docs/CHARTER.md`, `cadence-review.toml`, `src/review.rs`, `scripts/**`, and any change to who approves what, to a gate, or to this file. A PR that rewrites the rules can never approve itself. Scripts stay human by default; the only path exceptions are in [Script allowlist (auto-eligible)](#script-allowlist-auto-eligible).

### Script allowlist (auto-eligible)

Only these exact paths are eligible for `auto`:

- `scripts/measurements/README.md` — isolated board measurement procedure.
- `scripts/measurements/board-live.mjs` — isolated loopback board diagnostics. Its eligibility depends on the loopback port guard (`board-live.mjs:16-18`), which must be re-checked on every edit.
- `scripts/test-dup-report` — read-only test duplication analysis.
- `scripts/test-dup-report.txt` — historical duplication report, not a gate input.
- `scripts/auto-stage.py` — candidate selector and receipt writer. It runs in the `stage` job of both automatic and manual `workflow_dispatch` runs; in a manual run its only possible effect is a failure. Its candidate check is not what promotion relies on: the `promote` job re-verifies the artifact itself (`scripts/delivery-candidate.py prepare`) and runs only on manual `workflow_dispatch` behind the `production` environment's required reviewers. Changes to `scripts/delivery-candidate.py`, to the `promote` job, or to `.github/**` stay human.

Each path must satisfy ALL four criteria:

1. It cannot change who approves anything, what a required CI check verifies, or whether a required check passes.
2. It cannot change what is built, attested, released, published or promoted, or verify artifacts or attestations that promotion relies on. A check that promotion repeats independently does not count as one promotion relies on.
3. It cannot touch production, the installed binary, the tracker, secrets or credentials, or install software on a host.
4. Any effect is behind a human-class control downstream: diagnostics do not authorize delivery, and changes to gates, releases or production still require operator approval.

A PR is auto-eligible under this allowlist only when every `scripts/**` path it touches is allowlisted, the changes preserve all four criteria, and no other human trigger applies (for example, `.github/**` stays human under trigger 4). New paths and new gate or delivery wiring are not implicitly allowlisted: a PR that gives an allowlisted script a gate, release or promotion caller must remove its allowlist entry in the same PR. Changing this allowlist is itself trigger 7 (`human`). The two-independent-review requirement is unchanged.

## Class `auto` — ops-1 merges on its own
Everything else, provided ALL hold: qa-1 verdict `pass` on the exact head SHA; CI green on that head; the combined-tree gate (train) green including one full integration suite under the suite lock; net-deletion check clean; the author is frozen; no secret-looking string in the PR body or diff.

## Who decides
qa-1 states `Risk: auto` or `Risk: human (<trigger numbers>)` in every verdict with one line of reasons. ops-1 re-checks mechanically (paths touched, `Cargo.toml`/workflow diffs, line count, round count). If either says `human`, it is `human`. When unsure, `human`.

## After an auto merge
ops-1 runs the post-merge list, then a smoke check on the live system (`cadence --version`, `cadence status`, board 200, `cadence doctor`), and sends fable-cc one line: `merged #N <sha> (auto): <title>; tree ok; smoke ok`. If the smoke check fails: open a revert PR immediately (`gh pr create` with `git revert`), mark it `human`, and escalate to fable-cc. Do not merge the revert without approval.
