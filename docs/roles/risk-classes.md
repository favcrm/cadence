# Merge risk classes (shared by qa-1 and ops-1) — operator decision 2026-09-19

Goal: autonomous delivery. Routine PRs merge without a human; only substantial changes wait for the operator.

## Class `human` — the operator approves (any ONE trigger is enough)
1. **Trust boundary:** turn tokens, generations, session binding or ownership proofs, adoption/recovery (`recover()`, `open_adopted`, `verify_ownership`, `resolve_session`), the approval broker, permission modes, bypass handling, auth or identity (`request_identity`, `write_caller`, `tailnet_proof`, tailnet).
2. **Data and deletion:** schema migrations or any change to the store version; code that deletes or rewrites user data (worktree/branch removal, `issue finish`, `agent gc`, tracker write path, memory writes); anything that can lose commits.
3. **Security:** fixes for a leak or vulnerability, secret handling, redaction logic.
4. **Supply chain and CI:** new or upgraded dependencies (`Cargo.toml`, `Cargo.lock`, `ui/package.json`), `.github/workflows/*`, scripts that post statuses.
5. **Size or contention:** more than 1500 changed lines (excluding fixtures and lockfile churn), a third review round, or an unresolved reviewer–author disagreement.
6. **Outward or fleet-wide actions** (see also `docs/CHARTER.md` non-goals): releases, tags, repo settings, anything posted publicly other than the PR merge itself; a `daemon restart` that is not `--when-idle`; killing processes cadence did not start; any `--force`.

7. **The rules and the gates themselves:** `docs/roles/*`, `docs/CHARTER.md`, `cadence-review.toml`, `src/review.rs`, `scripts/**`, and any change to who approves what, to a gate, or to this file. A PR that rewrites the rules can never approve itself. Scripts stay human by default; the only path exceptions are in [Script allowlist (auto-eligible)](#script-allowlist-auto-eligible).

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

A PR is auto-eligible under this allowlist only when every `scripts/**` path it touches is allowlisted, the changes preserve all four criteria, and no other human trigger applies (for example, `.github/**` stays human under trigger 4). New paths and new gate or delivery wiring are not implicitly allowlisted: a PR that gives an allowlisted script a gate, release or promotion caller must remove its allowlist entry in the same PR. Changing this allowlist is itself trigger 7 (`human`). The review count follows AGENTS.md "Review" (CAD-1104): one independent review plus the operator's approval for `human` diffs, and two reviews only for triggers 1 and 3.

## Class `delegated` — a designated agent approves (CAD-918, operator decision 2026-10-01)

Class `human` splits in two. These stay `human`, and the operator approves them at merge: trigger 1 (trust boundary and identity), trigger 3 (fixes for an actual leak or vulnerability, redaction logic), trigger 4 (supply chain and CI), trigger 6 (outward or fleet actions, production rollouts), trigger 7 (the rules and gates, including this section), and from trigger 2 schema migrations and store-version changes.

These become `delegated`: the rest of trigger 2 (data and deletion paths, the tracker write path, memory writes); trigger 5 (size and review rounds); and a ticket whose only `human` trigger is a schema or store-version change the operator pre-approved at ticket time (`cadence audit approve --issue <ID> --action scope`), when both reviewers state that the PR stays within that scope, name the pre-approval id, and add no new trigger. Only schema changes are pre-approvable; triggers 1, 3, 4, 6 and 7 never are.

A designated agent approves a `delegated` PR with `cadence audit approve --pr <n> --head <full sha> --delegated [--scope <id>] --source "<notes>"`, run from its own pane. The daemon refuses it unless every safeguard holds:
1. Two PASS verdict notes for the ticket and PR, each pinned to the exact head, from two distinct reviewers, compared by the alias their `From:` names. Neither reviewer is an author (the ticket's owner, the loop's worker, the PR's GitHub login) or the approver. No verdict note on that head may state anything but `auto` or `delegated`.
2. CI green at that head: every check the base branch requires has a completed, successful run from its required app (GitHub Actions), inside the check suite of a `pull_request` run of `.github/workflows/ci.yml` for that head; a same-named run from any other workflow counts for nothing. Commit statuses count for nothing, and a `qa-verdict` in any state but success refuses.
3. Any gate change carries an independent acceptance check proving the bad case is refused (AGENTS.md "Gates and security work"). Reviewers check this, and their PASS states it.
4. A mechanical path check, an allowlist that fails closed. Every changed path, both sides of a rename included, must be `delegable` and in no trigger list; a schema path passes only under a live scope pre-approval. Anything else needs the operator. The lists live in one place, [`docs/roles/risk-paths.toml`](risk-paths.toml), which the binary compiles in, and are enumerated below.
5. The approval is recorded as `delegated:<alias>`. The alias comes from the caller's connection (the CAD-411 derivation), never from a flag, so the operator, a detached child and an unprovable caller are all refused. Only an agent the operator designated for the ticket's project may record it (`cadence audit designate <alias> --project <key>`, operator only, listed by `cadence audit designations`), and only for that project's own repo.
6. `cadence audit digest [--since 24h]` lists delegated approvals with both reviewers, a revoke command, and a revert command once merged. `cadence audit revoke` withdraws a delegated approval, and a revoked head can then be approved only by the operator.

A scope pre-approval binds to the ticket's current text and its lane branch (the PR's head branch must be one the ticket's `issue start` recorded), and the first delegated approval consumes it.

The verdict notes are files the agents' uid can write: the author or the approver can write both notes, and the daemon cannot tell. Delegated approvals are therefore evidence only, and nothing may enforce merges from them until CAD-814 slice 2 (daemon-attested receipts). `cadence audit` reports them apart from operator approvals, and a delegated approval never satisfies a `human` merge.

### Mechanical path lists

`delegable` names files, not directories, wherever code is involved: nothing that builds, runs in the browser (`ui/**` is trigger 1: the board runs with the operator's session) or acts on the host. Gate inputs and the tests that prove gates are trigger 7.

- **delegable**: `src/issue/blocked.rs`, `src/issue/cli.rs`, `src/issue/context.rs`, `src/issue/groom.rs`, `src/issue/history.rs`, `src/issue/line_times.rs`, `src/issue/lint.rs`, `src/issue/retro.rs`, `src/issue/sprint.rs`, `src/issue/summary.rs`, `src/issue/sync.rs`, `src/issue/time.rs`, `src/issue/write.rs`, `src/cli/issue.rs`, `src/cli/milestone.rs`, `src/cli/project.rs`, `src/cli/status.rs`, `src/cli/overview.rs`, `src/cli/events.rs`, `src/cli/help.rs`, `docs/**`
- **trigger1**: `src/peer.rs`, `src/daemon.rs`, `src/daemon/identity.rs`, `src/daemon/caller_rule.rs`, `src/daemon/slots_rpc.rs`, `src/adapter/pty/**`, `src/ui.rs`, `src/ui/**`, `ui/**`, `src/operator_auth.rs`, `src/tailnet_proof.rs`, `src/test_seam.rs`, `src/master_perm.rs`, `src/agent_uid/**`, `src/cli/agent_uid.rs`, `src/daemon/operator_rpc.rs`, `src/daemon/supervisor_grant.rs`, `src/daemon/installer_enrollment_wire.rs`, `src/platform/custody.rs`, `src/cli/platform.rs`, `src/bin/cadence-agent-exec/**`, `src/cli_actor.rs`, `src/board_identity.rs`, `src/device_login.rs`, `src/remote_auth.rs`, `src/remote_cli.rs`, `src/remote_enrollment.rs`
- **schema**: `src/store/schema.rs`, `src/remote_result_outbox.rs`
- **trigger3**: `src/secret/**`, `src/cli/secret.rs`, `src/cli/connection.rs`, `src/session.rs`, `src/backup/mod.rs`, `src/continuity.rs`, `src/doctor/host/util.rs`, `src/issue/report.rs`, `src/issue/areas.rs`
- **trigger4**: `Cargo.toml`, `Cargo.lock`, `build.rs`, `rust-toolchain.toml`, `.cargo/**`, `.config/**`, `**/package.json`, `**/pnpm-lock.yaml`, `.github/**`, `scripts/**`, `**/.github/**`, `**/.cargo/**`, `config/**`, `**/package-lock.json`, `**/yarn.lock`, `**/bun.lockb`, `**/.npmrc`, `**/build.rs`, `**/rust-toolchain.toml`, `**/Cargo.toml`, `**/Cargo.lock`
- **trigger6**: `src/rollout.rs`, `src/update.rs`, `src/upgrade.rs`, `src/cli/rollout.rs`, `src/cli/daemon.rs`, `src/cli/update.rs`, `src/cli/upgrade.rs`
- **trigger7**: `docs/roles/**`, `docs/CHARTER.md`, `docs/AUDIT.md`, `docs/design/**`, `docs/cadence/**`, `docs/START-HERE.md`, `docs/ARCHITECTURE.md`, `docs/BOARD.md`, `AGENTS.md`, `cadence-review.toml`, `clippy.toml`, `src/review.rs`, `src/review/**`, `src/audit.rs`, `src/audit/**`, `src/delegation.rs`, `src/delegation/**`, `src/daemon/approvals_rpc.rs`, `src/store/events.rs`, `src/overview.rs`, `src/delivery.rs`, `src/issue/delivery_policy.rs`, `src/issue/model.rs`, `src/issue/board.rs`, `src/issue/parse.rs`, `src/cli/audit.rs`, `src/cli/mod.rs`, `scripts/**`, `tests/**`

## Class `auto` — ops-1 merges on its own
Everything else, provided ALL hold: qa-1 verdict `pass` on the exact head SHA; CI green on that head; the combined-tree gate (train) green including one full integration suite under the suite lock; net-deletion check clean; the author is frozen; no secret-looking string in the PR body or diff.

## Who decides
qa-1 states `Risk: auto`, `Risk: delegated (<triggers>)` or `Risk: human (<trigger numbers>)` in every verdict with one line of reasons. ops-1 re-checks mechanically (paths touched, `Cargo.toml`/workflow diffs, line count, round count). If either says `human`, it is `human`. When unsure, `human`.

### Reviewer count on a solo-operator lane
For a two-review PR (AGENTS.md "Review", CAD-1099), the default is two distinct independent reviewers (Standards and
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

### Review count by diff (CAD-957)
A PR whose every change qualifies under the one-review allowlist
(`docs/roles/one-review-paths.toml`, match rules in its header) needs
one independent review covering standards and spec. Everything else keeps two. This changes only the count:
a `human` trigger still needs the operator, and the file is itself trigger 7.

## After an auto merge
ops-1 runs the post-merge list, then a smoke check on the live system (`cadence --version`, `cadence status`, board 200, `cadence doctor`), and sends fable-cc one line: `merged #N <sha> (auto): <title>; tree ok; smoke ok`. If the smoke check fails: open a revert PR immediately (`gh pr create` with `git revert`), mark it `human`, and escalate to fable-cc. Do not merge the revert without approval.
