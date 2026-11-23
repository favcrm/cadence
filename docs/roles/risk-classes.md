# Merge risk classes

The aim: routine PRs need the least evidence, and only substantial changes wait for the operator's approval. On a hosted org the operator always runs the enqueue. `cadence delivery requirements` computes what a PR needs; the review and delivery route is AGENTS.md "Review".

## Class `human` — the operator approves (any ONE trigger is enough)
When unsure whether a trigger applies, `human`. A reviewer names the triggers it finds in `Risk: human (<n>, …)`, and any `Risk:` that is not `auto` forces the operator. The shared evaluator also enforces the path and size triggers mechanically: paths in the `operator` table and triggers 2, 4, 6 and 7, and a diff over 1500 changed lines, force the operator. Triggers 1 and 3 reach the operator through the reviewer: a reviewer who sees one writes `Risk: human (1)` or `(3)`, which forces the operator. Their path lists alone put the PR in the strict class (two separate reviews) without a mechanical operator floor, and a scope approval never covers them.
1. **Trust boundary:** turn tokens, generations, session binding or ownership proofs, adoption/recovery (`recover()`, `open_adopted`, `verify_ownership`, `resolve_session`), the approval broker, permission modes, bypass handling, auth or identity (`request_identity`, `write_caller`, `tailnet_proof`, tailnet).
2. **Data and deletion:** schema migrations or any change to the store version; code that deletes or rewrites user data (worktree/branch removal, `issue finish`, `agent gc`, tracker write path, memory writes); anything that can lose commits.
3. **Security:** fixes for a leak or vulnerability, secret handling, redaction logic.
4. **Supply chain and CI:** new or upgraded dependencies (`Cargo.toml`, `Cargo.lock`, `ui/package.json`), `.github/workflows/*`, scripts that post statuses.
5. **Size or contention:** more than 1500 changed lines (excluding fixtures and lockfile churn), a third review round, or an unresolved reviewer–author disagreement.
6. **Outward or fleet-wide actions** (see also `docs/CHARTER.md` non-goals): releases, tags, repo settings, anything posted publicly other than the PR merge itself; a `daemon restart` that is not `--when-idle`; killing processes cadence did not start; any `--force`.

7. **The rules and the gates themselves:** `docs/roles/*`, `docs/CHARTER.md`, `cadence-review.toml`, `src/review.rs`, `scripts/**`, and any change to who approves what, to a gate, or to this file. `docs/roles/review-workflow-principals.toml` lists the workflow review principals the gate counts, and `docs/roles/workflow-auto-paths.toml` the only paths on which workflow reviews alone may merge. The gate reads both from the live tip of `main`, so a PR can never add its own reviewer or path. A workflow review row may say `Risk: auto` (a prompt-injected model can write that), so a PR with a workflow review needs the operator's approval unless every changed path is a plain add or modify on `docs/roles/workflow-auto-paths.toml` (Markdown documentation under docs/), is on no human-class path, has both workflow reviews passing and trips no size or non-`auto` `Risk:` trigger. The gate's dry run reporting `class: auto` is the test; being on the list is not enough. Whatever the list says, these are never eligible: a hidden path, an agent-instruction file name (a basename starting `agents.`, `claude.`, `gemini.`, `agent.`, `warp.`, `qwen.`, `crush.` or `copilot-instructions.`), an odd path name (a control, format or line separator character, a space other than the ASCII space, or a segment ending in a dot or a space), a path the one-review list excludes, a trigger 1 or 3 path, and a name that differs only by case from a protected path. An unreadable list means the operator, and an empty one-review exclude, trigger 1 or trigger 3 list counts as unreadable, here and for a ticket scope approval. `src/delivery/**` is trigger 7. So are `docs/INSTALL-AGENT.md` (the install command and the attestation trust root) and `docs/REMOTE-AUTH.md` (the credential diagnosis the binary points to): a wrong edit to either misleads an install or exposes a credential, so neither merges on workflow reviews alone. A PR that rewrites the rules can never approve itself. Scripts stay human by default; the only path exceptions are in [Script allowlist (auto-eligible)](#script-allowlist-auto-eligible).

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

A PR is auto-eligible under this allowlist only when every `scripts/**` path it touches is allowlisted, the changes preserve all four criteria, and no other human trigger applies (for example, `.github/**` stays human under trigger 4). New paths and new gate or delivery wiring are not implicitly allowlisted: a PR that gives an allowlisted script a gate, release or promotion caller must remove its allowlist entry in the same PR. Changing this allowlist is itself trigger 7 (`human`). Review counts follow AGENTS.md "Review".

### Mechanical path lists

`delegable` feeds only the daemon's `--delegated` approval check (`docs/AUDIT.md`), not the merge evaluator. It names files, not directories, wherever code is involved: nothing that builds, runs in the browser (`ui/**` is trigger 1: the board runs with the operator's session) or acts on the host. Gate inputs and the tests that prove gates are trigger 7.

- **delegable**: `src/issue/blocked.rs`, `src/issue/cli.rs`, `src/issue/context.rs`, `src/issue/groom.rs`, `src/issue/history.rs`, `src/issue/line_times.rs`, `src/issue/lint.rs`, `src/issue/retro.rs`, `src/issue/sprint.rs`, `src/issue/summary.rs`, `src/issue/sync.rs`, `src/issue/time.rs`, `src/issue/write.rs`, `src/cli/issue.rs`, `src/cli/milestone.rs`, `src/cli/project.rs`, `src/cli/status.rs`, `src/cli/overview.rs`, `src/cli/events.rs`, `src/cli/help.rs`, `docs/**`
- **trigger1**: `src/peer.rs`, `src/daemon.rs`, `src/daemon/identity.rs`, `src/daemon/caller_rule.rs`, `src/daemon/slots_rpc.rs`, `src/adapter/pty/**`, `src/ui.rs`, `src/ui/**`, `crates/cadence-ui/**`, `ui/**`, `src/operator_auth.rs`, `src/tailnet_proof.rs`, `src/test_seam.rs`, `src/master_perm.rs`, `src/agent_uid/**`, `src/cli/agent_uid.rs`, `src/daemon/operator_rpc.rs`, `src/daemon/supervisor_grant.rs`, `src/daemon/installer_enrollment_wire.rs`, `src/platform/custody.rs`, `src/cli/platform.rs`, `src/bin/cadence-agent-exec/**`, `src/cli_actor.rs`, `src/board_identity.rs`, `src/device_login.rs`, `src/remote_auth.rs`, `src/remote_cli.rs`, `src/remote_enrollment.rs`, `src/remote_cli/**`, `src/remote_enrollment/**`, `src/remote_*_acceptance.rs`, `src/runner.rs`
- **schema**: `src/store/schema.rs`, `src/remote_result_outbox.rs`
- **trigger3**: `src/secret/**`, `src/cli/secret.rs`, `src/cli/connection.rs`, `src/session.rs`, `src/backup/mod.rs`, `src/continuity.rs`, `src/doctor/host/util.rs`, `src/issue/report.rs`, `src/issue/areas.rs`
- **trigger4**: `Cargo.toml`, `Cargo.lock`, `build.rs`, `rust-toolchain.toml`, `.cargo/**`, `.config/**`, `**/package.json`, `**/pnpm-lock.yaml`, `.github/**`, `scripts/**`, `**/.github/**`, `**/.cargo/**`, `config/**`, `**/package-lock.json`, `**/yarn.lock`, `**/bun.lock`, `**/bun.lockb`, `**/.npmrc`, `**/build.rs`, `**/rust-toolchain.toml`, `**/Cargo.toml`, `**/Cargo.lock`
- **trigger6**: `src/rollout.rs`, `src/update.rs`, `src/upgrade.rs`, `src/cli/rollout.rs`, `src/cli/daemon.rs`, `src/cli/update.rs`, `src/cli/upgrade.rs`
- **trigger7**: `docs/roles/**`, `docs/CHARTER.md`, `docs/AUDIT.md`, `docs/design/**`, `docs/cadence/**`, `docs/START-HERE.md`, `docs/ARCHITECTURE.md`, `docs/BOARD.md`, `docs/INSTALL-AGENT.md`, `docs/REMOTE-AUTH.md`, `AGENTS.md`, `cadence-review.toml`, `clippy.toml`, `src/review.rs`, `src/review/**`, `src/audit/**`, `src/delegation.rs`, `src/delegation/**`, `src/daemon/approvals_rpc.rs`, `src/store/events.rs`, `src/overview.rs`, `src/delivery.rs`, `src/delivery/**`, `src/issue/delivery_policy.rs`, `src/issue/delivery_requirements.rs`, `src/issue/model.rs`, `src/issue/board.rs`, `src/issue/parse.rs`, `src/cli/audit.rs`, `src/cli/mod.rs`, `scripts/**`, `tests/**`

The shared requirements engine also reads three tables of `risk-paths.toml` that hold this repository's own rules as data (the engine names no repository's paths): `operator` (further operator-class build, release, rule and gate files), `browser_qa` (paths whose change needs a Browser QA pass) and `routine` (ordinary documentation the routine class admits). Each is required; a missing or malformed one fails closed to the strict floor with no Browser QA. Editing them is trigger 7 like the rest of `docs/roles/**`.

The evaluator reads these lists at the PR's anchor, never the PR's own copy (AGENTS.md "Where the rules live"). The copy compiled into the binary serves only the daemon's `--delegated` approval check, and a test pins it to this file.

## Class `auto`
Everything else.

## Who counts as a reviewer

The review standard (fresh-session review), the hosted route and what the gate
enforces are AGENTS.md "Review", the one home. The operator's approval of the
exact head (`cadence audit approve`, operator-connection-only, an
`approval_recorded` event on the `audit:approvals` stream bound to the head; a
ticket comment or note is never approval evidence) never stands in for a review
pass and never lowers the risk class. It does not waive Browser QA: it can accept
the PR author's exact-head Browser QA pass. A change to the review rule is class
`human` (trigger 7) and can never approve itself.

## Review count by diff
A PR whose every change qualifies under the one-review allowlist
(`docs/roles/one-review-paths.toml`, match rules in its header) needs
one fresh-session review covering standards and spec; everything else in the
strict class needs two separate passes (Standards and Spec/security). The
evaluator counts zero reviews for `routine` and one for `consequential`. This changes only the count; the operator floor is separate, and the file is itself trigger 7.
