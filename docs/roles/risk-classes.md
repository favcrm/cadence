# Merge risk classes — operator decision 2026-09-19

The aim: routine PRs merge without a human, and only substantial changes wait for the operator. `cadence delivery requirements` and `scripts/enqueue-reviewed` compute what a PR needs.

## Class `human` — the operator approves (any ONE trigger is enough)
When unsure whether a trigger applies, `human`.
1. **Trust boundary:** turn tokens, generations, session binding or ownership proofs, adoption/recovery (`recover()`, `open_adopted`, `verify_ownership`, `resolve_session`), the approval broker, permission modes, bypass handling, auth or identity (`request_identity`, `write_caller`, `tailnet_proof`, tailnet).
2. **Data and deletion:** schema migrations or any change to the store version; code that deletes or rewrites user data (worktree/branch removal, `issue finish`, `agent gc`, tracker write path, memory writes); anything that can lose commits.
3. **Security:** fixes for a leak or vulnerability, secret handling, redaction logic.
4. **Supply chain and CI:** new or upgraded dependencies (`Cargo.toml`, `Cargo.lock`, `ui/package.json`), `.github/workflows/*`, scripts that post statuses.
5. **Size or contention:** more than 1500 changed lines (excluding fixtures and lockfile churn), a third review round, or an unresolved reviewer–author disagreement.
6. **Outward or fleet-wide actions** (see also `docs/CHARTER.md` non-goals): releases, tags, repo settings, anything posted publicly other than the PR merge itself; a `daemon restart` that is not `--when-idle`; killing processes cadence did not start; any `--force`.

7. **The rules and the gates themselves:** `docs/roles/*`, `docs/CHARTER.md`, `cadence-review.toml`, `src/review.rs`, `scripts/**`, and any change to who approves what, to a gate, or to this file. `docs/roles/review-workflow-principals.toml` is the allowlist of workflow review principals the gate counts (CAD-2081); it is read from the live tip of `main`, so a PR can never add its own reviewer. A workflow review row may say `Risk: auto` (a prompt-injected model can write that), so a PR with a workflow review needs the operator's approval unless every changed path is a plain add or modify matching `docs/roles/workflow-auto-paths.toml` (read from the live tip of `main`; a positive allowlist that is empty today, so the operator approves every merge and workflow rows only satisfy the reviews; enabling a path is a later, deliberate, reviewed change, and a hidden path or an agents., claude. or gemini. file is never eligible). `src/delivery/**` is trigger 7. A PR that rewrites the rules can never approve itself. Scripts stay human by default; the only path exceptions are in [Script allowlist (auto-eligible)](#script-allowlist-auto-eligible).

### Script allowlist (auto-eligible)

Only these exact paths are eligible for `auto`:

- `scripts/measurements/README.md` — isolated board measurement procedure.
- `scripts/measurements/board-live.mjs` — isolated loopback board diagnostics. Its eligibility depends on the loopback port guard (`board-live.mjs:16-18`), which must be re-checked on every edit.
- `scripts/test-dup-report` — read-only test duplication analysis.
- `scripts/auto-stage.py` — candidate selector and receipt writer. It runs in the `stage` job of both automatic and manual `workflow_dispatch` runs; in a manual run its only possible effect is a failure. Its candidate check is not what promotion relies on: the `promote` job re-verifies the artifact itself (`scripts/delivery-candidate.py prepare`) and runs only on manual `workflow_dispatch` behind the `production` environment's required reviewers. Changes to `scripts/delivery-candidate.py`, to the `promote` job, or to `.github/**` stay human.

Each path must satisfy ALL four criteria:

1. It cannot change who approves anything, what a required CI check verifies, or whether a required check passes.
2. It cannot change what is built, attested, released, published or promoted, or verify artifacts or attestations that promotion relies on. A check that promotion repeats independently does not count as one promotion relies on.
3. It cannot touch production, the installed binary, the tracker, secrets or credentials, or install software on a host.
4. Any effect is behind a human-class control downstream: diagnostics do not authorize delivery, and changes to gates, releases or production still require operator approval.

A PR is auto-eligible under this allowlist only when every `scripts/**` path it touches is allowlisted, the changes preserve all four criteria, and no other human trigger applies (for example, `.github/**` stays human under trigger 4). New paths and new gate or delivery wiring are not implicitly allowlisted: a PR that gives an allowlisted script a gate, release or promotion caller must remove its allowlist entry in the same PR. Changing this allowlist is itself trigger 7 (`human`). The review count follows AGENTS.md "Review" (CAD-1104): one fresh-session review plus the operator's approval for `human` diffs, and two separate passes only for triggers 1 and 3.

### Mechanical path lists

`delegable` feeds only the daemon's `--delegated` approval check (`docs/AUDIT.md`), not the merge evaluator. It names files, not directories, wherever code is involved: nothing that builds, runs in the browser (`ui/**` is trigger 1: the board runs with the operator's session) or acts on the host. Gate inputs and the tests that prove gates are trigger 7.

- **delegable**: `src/issue/blocked.rs`, `src/issue/cli.rs`, `src/issue/context.rs`, `src/issue/groom.rs`, `src/issue/history.rs`, `src/issue/line_times.rs`, `src/issue/lint.rs`, `src/issue/retro.rs`, `src/issue/sprint.rs`, `src/issue/summary.rs`, `src/issue/sync.rs`, `src/issue/time.rs`, `src/issue/write.rs`, `src/cli/issue.rs`, `src/cli/milestone.rs`, `src/cli/project.rs`, `src/cli/status.rs`, `src/cli/overview.rs`, `src/cli/events.rs`, `src/cli/help.rs`, `docs/**`
- **trigger1**: `src/peer.rs`, `src/daemon.rs`, `src/daemon/identity.rs`, `src/daemon/caller_rule.rs`, `src/daemon/slots_rpc.rs`, `src/adapter/pty/**`, `src/ui.rs`, `src/ui/**`, `ui/**`, `src/operator_auth.rs`, `src/tailnet_proof.rs`, `src/test_seam.rs`, `src/master_perm.rs`, `src/agent_uid/**`, `src/cli/agent_uid.rs`, `src/daemon/operator_rpc.rs`, `src/daemon/supervisor_grant.rs`, `src/daemon/installer_enrollment_wire.rs`, `src/platform/custody.rs`, `src/cli/platform.rs`, `src/bin/cadence-agent-exec/**`, `src/cli_actor.rs`, `src/board_identity.rs`, `src/device_login.rs`, `src/remote_auth.rs`, `src/remote_cli.rs`, `src/remote_enrollment.rs`, `src/remote_cli/**`, `src/remote_enrollment/**`, `src/remote_*_acceptance.rs`, `src/runner.rs`
- **schema**: `src/store/schema.rs`, `src/remote_result_outbox.rs`
- **trigger3**: `src/secret/**`, `src/cli/secret.rs`, `src/cli/connection.rs`, `src/session.rs`, `src/backup/mod.rs`, `src/continuity.rs`, `src/doctor/host/util.rs`, `src/issue/report.rs`, `src/issue/areas.rs`
- **trigger4**: `Cargo.toml`, `Cargo.lock`, `build.rs`, `rust-toolchain.toml`, `.cargo/**`, `.config/**`, `**/package.json`, `**/pnpm-lock.yaml`, `.github/**`, `scripts/**`, `**/.github/**`, `**/.cargo/**`, `config/**`, `**/package-lock.json`, `**/yarn.lock`, `**/bun.lock`, `**/bun.lockb`, `**/.npmrc`, `**/build.rs`, `**/rust-toolchain.toml`, `**/Cargo.toml`, `**/Cargo.lock`
- **trigger6**: `src/rollout.rs`, `src/update.rs`, `src/upgrade.rs`, `src/cli/rollout.rs`, `src/cli/daemon.rs`, `src/cli/update.rs`, `src/cli/upgrade.rs`
- **trigger7**: `docs/roles/**`, `docs/CHARTER.md`, `docs/AUDIT.md`, `docs/design/**`, `docs/cadence/**`, `docs/START-HERE.md`, `docs/ARCHITECTURE.md`, `docs/BOARD.md`, `AGENTS.md`, `cadence-review.toml`, `clippy.toml`, `src/review.rs`, `src/review/**`, `src/audit/**`, `src/delegation.rs`, `src/delegation/**`, `src/daemon/approvals_rpc.rs`, `src/store/events.rs`, `src/overview.rs`, `src/delivery.rs`, `src/delivery/**`, `src/issue/delivery_policy.rs`, `src/issue/delivery_requirements.rs`, `src/issue/model.rs`, `src/issue/board.rs`, `src/issue/parse.rs`, `src/cli/audit.rs`, `src/cli/mod.rs`, `scripts/**`, `tests/**`

## Class `auto`
Everything else.

## Who counts as a reviewer: the fresh-session review standard
Operator decision 2026-11-03 (CAD-1946). A review is valid when a fresh-context
session that is not the author does it; a subagent qualifies. Fresh means it
inherits no conversation or context from the work: a forked subagent or a
resumed authoring session does not count, and its brief points at the change
without the author's conclusions. A second model vendor is optional, and a
separate human or account is not required. Records say "fresh-session review"
and never claim independent parties. For a two-review PR (AGENTS.md "Review",
CAD-1099) Standards and Spec/security remain two separate passes by two fresh
sessions; one pass never covers both. The operator's approval of the exact head
(`cadence audit approve --pr <n> --head <full sha> --action merge`,
operator-connection-only, an `approval_recorded` event on the `audit:approvals`
stream bound to the head; a ticket comment or note is never approval evidence)
still applies on `human`-class paths; this decision changes no approval rule
for send, publish, spend or deploy. It never stands in for a review pass and never lowers Browser QA, the
`qa-verdict` status binding or the risk class. A change to this rule is class
`human` (trigger 7) and can never approve itself.

**Not yet enforced.** `scripts/enqueue-reviewed` counts hosted verdicts by
attested principal and, since CAD-2049, refuses the author's principal; local
notes count by their self-declared `From:` alias, which is not attested. It
cannot tell a fresh session from the author under one login. A hosted `routine` PR
takes two workflow reviews, and operator approval while `workflow-auto-paths.toml`
does not list its paths, because a hosted board has no readiness record (CAD-2135). A hosted `ui/**` PR takes those two reviews, operator
approval and a `--browser-qa` comment naming its exact head (CAD-2173), or until it ends, the
temporary exception (`--exception --advisory ...`: exact-head
operator approval and two advisory links, standards and spec-security, plus browser-qa for a `ui/`
change; ends at
`EXCEPTION_ENDS` in `scripts/delivery-gate`, 2026-12-31). The exception only
checks that the advisory links are separate https URLs; it proves neither head
binding nor that Browser QA ran. Agents file no hosted verdict (AGENTS.md "Review"). The `solo_operator` delivery-profile
field still exists in code; it is not this standard. A workflow-issued reviewer
identity (CAD-2081) is counted by the gate and its workflows are live; their role is
in AGENTS.md "Review".

## Review count by diff (CAD-957)
Under the default lean policy, a PR whose every change qualifies under the one-review allowlist
(`docs/roles/one-review-paths.toml`, match rules in its header) needs
one fresh-session review covering standards and spec. Under `strict`, everything else keeps two. This changes only the count:
a `human` trigger still needs the operator, and the file is itself trigger 7.
