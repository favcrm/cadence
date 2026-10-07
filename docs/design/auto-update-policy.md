# CAD-1170 — Auto-update policy: findings, autonomy bounds, recommended tier

Research only. This document recommends a policy; it changes no mechanism.
Every claim cites the source that owns it.

## 1. What an update already is

`cadence update` (CAD-561) is already a single orchestrated pipeline in
`src/update.rs`. One command — or one board button — runs, in order
(`src/update.rs` module doc and `run_inner`):

1. **check** — resolve the approved production candidate, list merged PR
   titles since the linked release, compare the target's
   `SCHEMA_VERSION` against the store's, list the lease and in-flight
   turns as blockers (`update::check`, `CheckReport::lines`).
2. **lease** — auto-claim the rollout lease for the target, auto-release
   at the end (`take_lease`, `LEASE_TTL` = 1h).
3. **backup** — `cadence backup` outside the state dir, verified again
   right before install, recorded as the lease's backup receipt — the
   receipt is what later authorizes a schema crossing
   (`take_backup`; `rollout::authorize_migration` refuses a crossing
   without a matching receipt).
4. **install** — `upgrade::run` downloads the attested CI artifact
   side by side and moves the symlink only after seven checks pass
   (`src/upgrade.rs` module doc: on-main, `test` job on that exact sha,
   artifact present, sha256 match, manifest `source_sha`, `gh
   attestation verify` scoped to `ci.yml`/`refs/heads/main`/sha, and
   `--version` ends in `+<sha>` before the binary is ever run).
5. **drain** — `update_drain on` (operator-connection RPC,
   `src/daemon.rs:2646`; caller rule `update_drain` is operator-only and
   `update_status` is a plain read, `src/daemon/caller_rule.rs:94-100`)
   stops new turns; the pipeline waits at most `--drain` (default 10m,
   `DEFAULT_DRAIN`) listing exactly what it waits on. `--now` switches
   at once; interrupted turns resume via the existing adoption path.
6. **switch** — the *new* binary's `daemon restart --when-idle --ui`
   runs the restart; a non-zero exit is a warning, not a stop — only
   the health check decides (`RestartOutcome`, `restart_on`).
7. **health** — daemon and (when it was running) board must answer on
   the new build within 90s (`HEALTH_TIMEOUT`); transient probe errors
   retry until the deadline. On failure the pipeline repoints the link
   to the previous release, restarts, re-checks health, and tells the
   operator to `cadence restore <manifest>` when the schema moved.
8. **commit** — keep `DEFAULT_KEEP` (3) releases, prune the rest, lift
   the drain, release the lease.

Failure containment already present: `update.lock` flock serializes one
run per state dir (`RunLock`); `update.json` is a heartbeat-marked
pending-update file the daemon adopts only while the writer is the live
lease holder (`marker_is_the_lease_holders`, `src/daemon.rs:3553`), and
a marker stale >5min (`UPDATE_STALE_SECS`) stops draining the fleet — a
dead updater un-wedges itself. `update-progress.jsonl` is the run log
the board reads (`read_run_log`).

## 2. Who may act today

- **CLI**: `update_caller` (`src/cli/update.rs`) refuses inside any pane
  (`CADENCE_ALIAS` set), requires `--as operator:<name>` outside, and
  runs `rollout::require_operator` — `peer::operator_proof`
  (`src/peer.rs:258`): same uid, no registered pane, enrolled managed
  root, daemon descendant, or `CADENCE_ALIAS`/`CADENCE_RUNNER_ID`
  carrier on the `/proc` ancestry, session leader on the ancestry.
  Agents can never trigger an update.
- **Board**: `POST /api/update` and `/api/update/check` are
  `RouteClass::OperatorOnly` (`src/ui/operator.rs:349-350`). The board
  never runs the pipeline in-process — it spawns a detached
  `cadence update --as <UI_ACTOR>` helper with `setsid` and a scrubbed
  env (`src/ui/updates.rs:200-290`), so the restart killing the board
  cannot kill the update.
- **Lease verbs**: `rollout claim --as operator:*` must pass operator
  proof (`src/rollout.rs:1249-1260`). A *pane alias* may claim only
  under a live `rollout grant` (CAD-384); a `delegate:<alias>` only
  under a `staging delegate` grant scoped to a registered staging dir
  (CAD-1024). Production has no agent path to the lease.
- **Drain**: `update_drain` requires `operator_connection` on the RPC —
  the connection-level proof, not a label. `shutdown` admits the
  operator or the lease-holding granted pane (`granted_lease_holder`).
- **Schema one-way door**: `authorize_migration` refuses a store newer
  than the binary outright (`v > SCHEMA_VERSION` → "refusing to open"),
  and an older store migrates only with a matching backup receipt on a
  live lease — so rollback across a migration requires a *restore*, not
  a repoint (`update.rs` rollback path prints the restore command).

## 3. What staging/promotion already automates

`.github/workflows/staging.yml` + `scripts/auto-stage.py` (CAD-770):

- A **bounded selector** runs every 15min (`cron 7,22,37,52`) and on
  each successful main `ci.yml` `workflow_run`. It picks at most the
  newest eligible attested main candidate inside a 24h train window,
  pins sha/run/attempt/digest, refuses on a missing/ambiguous/unverified
  operator-pinned baseline (`config/production-baseline.json`), skips
  on in-flight runs and duplicates, and never stages from a PR or under
  reduced gates (`require-full-gates` on both control and candidate
  trees).
- The **stage** job (environment `staging`) re-verifies identity,
  digest and attestation, rehearse-migrates baseline→candidate with the
  real binaries, re-checks the digest after the journey, and writes a
  staging receipt.
- The **promote** job runs *only* on manual `workflow_dispatch` behind
  the `production` environment's required reviewers; it re-verifies the
  artifact after the approval wait and publishes the
  `production-candidate` receipt. The updater's `latest_green_main`
  resolves only a dispatch run whose `stage` AND `promote` jobs both
  succeeded and whose receipt binds the CI run/attempt/digest
  (`Gh::latest_green_main`, `parse_production_candidate`).

So today: **machines select and rehearse, a human approves the
production-candidate record, a human installs.** The first half is
already automated on a schedule; the second half is two human
decisions.

## 4. Decision inventory — which genuinely need a human

| Decision | Where it lives | Needs a human? |
|---|---|---|
| Choose the candidate | auto-stage selector | Already automated — bounded, pinned, refuses closed. |
| Rehearse against the previous release | staging `stage` job | Automated (migration/backup rehearsal on real binaries). |
| Record the production candidate | `promote` job | **Human today** — the production-environment approval is the only human review of "this sha may run production." Automatable only if replaced by an equivalent durable record (see §6). |
| When to interrupt the fleet | `--drain` bound + drain | **Human.** The drain bound (10m) trades a worker's in-flight turn against freshness; on timeout the update *switches anyway* and turns resume — which is fine for routine bumps and wrong during a long-running irreversible operation. A machine can pick a quiet window but cannot know that tonight's sweep must finish. |
| Schema one-way door | `authorize_migration` | **Human.** The crossing is authorized by the backup receipt, but the decision to accept "old binary can never reopen this store; rollback = restore" is judgment: is the backup verified *and restorable*, is anyone mid-turn whose resumption depends on post-migration state. `--check` surfaces `schema: migration 31 → 32`; nothing today requires a human to acknowledge it before install. |
| Identity for the install | `--as operator:<name>` + `operator_proof` | **Human today** — the lease holder and every audit line attribute to an operator. An auto tier needs a machine identity the audit model accepts (see §6). |
| Timing vs. production load | drain + waiters | **Partially human.** `check` reports waiters; a machine can defer while busy. But "load" is not just open turns — sweeps, scheduled agent work, an operator mid-incident. A windowed pre-approval encodes this judgment once instead of per-run. |
| Rollback decision on health fail | `health_wait` → repoint | **Already automated** — the pipeline rolls back itself on failed health. What is NOT automated: rollback when health passes but behavior is wrong (a bad build that answers health). That detection gap is a human's today and stays a human's in every tier below full-auto. |
| Who restarts the restarter | — | **Human, structurally.** If `cadence update` itself is broken (helper dies, wrong binary exec), nothing can fix it — the tooling is the patient. `RunLock`, the stale-marker un-wedge, `run_log_never_started`, and "the daemon is down → finish the update" (`finish_restart`) cover the common deaths; a wedged updater that never started needs a human or an external watchdog. |

## 5. Use cases

- **Operator-absent windows**: the real driver. Today a security fix
  merged Friday night waits for the operator to stage-approve and then
  run `cadence update`. The staging selector already covers selection;
  the gap is promotion approval + install. A windowed pre-approval
  ("install any security-labeled candidate between 02:00–05:00 local,
  if the fleet drains within 10m") covers this without ceding judgment.
- **Urgent security patches**: the case *for* automation and the case
  where it is most dangerous — a security fix is trigger 3 (human) at
  merge time (risk-classes trigger 3), so the PR itself got operator
  eyes; extending that to "the operator pre-authorized installing
  security-labeled heads" is defensible. Blast radius if wrong: an
  unattended bad security patch takes production down at 3am and the
  auto-rollback covers only *health* failures, not behavioral ones.
- **Single-host vs fleet**: this repo serves one production host;
  trigger 6 (fleet) exists because `rollout`/`update`/`upgrade` are the
  fleet-facing modules. A fleet would make canary rollout meaningful;
  on one host "canary" collapses to "install, watch, roll back" — which
  the pipeline already does. Fleet canary is a **non-goal** for the
  recommended tier.
- **Scheduled windows**: same shape as operator-absent — a pre-approved
  window is the mechanism, whether the absence is nightly or a
  vacation.
- **Daemon-down recovery**: `update` already finishes a half-done
  update (`finish_restart`: link moved but daemon on old build or no
  daemon answering → skip backup/install, drain, switch, health-check).
  Who restarts the restarter: nobody — by design. The mitigation that
  exists is the stale-marker un-wedge plus the idempotent re-run; the
  residual is an external supervisor noticing "production has been
  drained for N minutes with no progress" and paging the operator.
  That supervisor is a **mechanism delta**, not a tier.
- **Board down mid-update**: covered — detached helper outlives the
  board; the restarted board re-reads the same progress file.

## 6. Tier evaluation

| Tier | Trigger | Human override | Audit story | Rollback story | Blast radius if wrong |
|---|---|---|---|---|---|
| **T0 notify-only** (status quo + surfacing) | `update --check` output on the board Update card | n/a — human runs everything | Existing: lease rows, gate log, run log | Human `update --rollback` | None new. Exists today. |
| **T1 auto-stage** | Selector tick / green main run | Baseline pin + `production` env approval on promote | Selection + staging receipts per run | n/a — nothing installed | **Already exists and is bounded.** Confirmed: `scripts/auto-stage.py` refuses closed; promote stays manual dispatch + env approval. |
| **T2 auto-install, security-labeled** | A promoted candidate whose PR set is entirely security-labeled | Operator `update --rollback`; revoke the label | Needs a machine install identity the audit accepts; every run attributable | Health-fail rollback exists; behavioral-bad-build rollback does NOT | Wrong-label or bad-patch → unattended production change. Largest marginal risk per unit of coverage. |
| **T3 windowed auto-install, pre-approved** | A promoted candidate + inside an operator-declared window (e.g. nightly 02:00–05:00, fleet drained) | Window scope (ttl, schema-crossing exclusion, max one run); operator revokes the grant | Pre-authorized lease bound to window; machine identity `operator:auto` or a new `delegate:`-class holder; receipts | Same as T2 + window refuses schema crossings and non-drained fleets | Bounded: only runs inside the declared window, only on promoted candidates, never across a schema door. |
| **T4 full auto** | Every promoted candidate installs when green | `update --rollback` | Machine identity; promotion approval is the only gate | Same gaps as T2 | Largest: installs anything promoted, any time, including schema crossings unless explicitly refused. Removes the last human timing judgment. |

## 7. Mechanism deltas each tier needs

Things that do **not** exist today and a tier would require:

- **M1 — a machine install identity.** Every mutation path
  (`update_caller`, `update_drain`, `rollout claim`, `shutdown`)
  requires operator proof or a granted alias. An auto-installer needs a
  new caller shape — e.g. `machine:auto-update` or a `delegate:`-class
  grant scoped to the production state dir with an op like
  `update_install` — that the daemon's caller rule and
  `operator_connection`-gated verbs can admit *without* relaxing the
  operator proof for anything else. This is the inverse of CAD-1024
  (which grants staging dirs to delegates); here the grant must never
  be issuable for production except by an explicit operator action with
  the same connection-bound recording as `audit approve`.
- **M2 — pre-authorized window, lease-bound.** An operator-declared
  record (in the store or a signed file) naming: window start/end (or
  cron), allowed target set (latest promoted candidate only), a hard
  refusal list (schema crossings, fleet not drained within bound, lease
  held by another identity, a `human`-trigger PR in the changeset),
  TTL, and revocation. The installer claims the lease *as the machine
  identity* and the lease row names the window grant, so
  `rollout status` and the audit both show why the machine held it.
- **M3 — health-check-gated auto-rollback: mostly exists.** The
  pipeline already repoints + restarts + re-checks on health failure.
  The delta is *detection width*: health today = "daemon and board
  answer on the new build." A stronger gate (a post-switch smoke —
  `cadence status` answers, agent adoption count sane) is a small,
  separable ticket.
- **M4 — update-of-the-updater.** The installer binary is itself the
  thing being replaced. Today's ordering handles it: the *new* binary
  runs the restart (`upgrade::run` installs side-by-side before the
  link moves; `restart_on` execs the new binary). For an unattended
  tier the residual is: the auto-installer must run from a *pinned,
  previously-installed* binary — never from the just-installed one —
  and must be restartable by an external supervisor (cron/systemd
  path that does NOT carry operator proof needs its own admission rule,
  which is the hard part of M1).
- **M5 — security labeling.** No "security" label exists on candidates.
  T2 needs a trusted signal (PR title/label propagated through the
  staging receipt into `production-candidate.json`) — anything less is
  spoofable metadata.
- **M6 — failure-visibility surface.** An unattended tier needs the
  run log surfaced where a human will see it (board banner exists via
  `update.json`; a notification path — message to the operator — does
  not exist as a push channel).

## 8. Recommendation

**Recommend Tier 3 — windowed auto-install with operator pre-approval —
as the next tier after the confirmed-existing T1, and explicitly NOT
T2 or T4 now.**

Rationale:

- The two human decisions that remain are *promotion approval* and
  *install*. T3 keeps the first exactly as today (the
  `production`-environment approval remains the durable human record
  — no new mechanism needed there) and automates only the second,
  inside an operator-declared window. That matches the actual pain:
  the operator must be awake at a safe hour, not that they must judge
  each build — they already judged it at promote time.
- T2 (security-labeled auto-install) is *higher* risk than T3 despite
  sounding narrower: it fires at arbitrary times (a security patch does
  not wait for 02:00), needs a new trusted label path (M5), and its
  whole point is skipping the calm-window checks that make T3 safe. If
  urgency matters, the operator can still run `cadence update` by hand
  — T3 does not remove the manual path.
- T4 removes the timing judgment entirely; the drain-vs-interrupt and
  schema-door rows in §4 are the proof that judgment is not yet
  encodable. Revisit only after T3 runs for a while *and* a
  post-switch behavioral smoke (M3 delta) exists.

T3 trigger/override/audit/rollback:

- **Trigger**: latest `production-candidate` exists (promote already
  approved), current time inside a live operator-declared window,
  `check` reports no schema migration, no foreign lease, fleet drains
  within the bound. Any unmet condition → skip with a receipt, never
  retry inside the same window tick.
- **Override**: operator revokes the window grant (one command);
  `update --rollback` unchanged; the window grant never admits a schema
  crossing (hard refusal — that stays human forever, see non-goals).
- **Audit**: lease holder = machine identity carrying the window-grant
  id; `update.json`/`update-progress.jsonl`/gate log unchanged in
  shape; the window grant is recorded on the audit approvals stream
  like a scope approval (CAD-1106 shape: operator-connection-bound,
  digest of the declared window, revocable).
- **Rollback**: existing health-fail auto-rollback; schema crossings
  are refused rather than rolled back (a crossing is never attempted
  unattended, so no unattended restore is ever needed).

## 9. Risk classification

This document: **human (7)** at merge, not auto. `docs/design/**` is on
the `[trigger7]` list of `docs/roles/risk-paths.toml` (the rules and
their inputs), and `docs/roles/risk-classes.md` trigger 7 covers "any
change to ... a gate, or to this file" — a design doc that a future
auto-update mechanism will be reviewed against sits in that class
mechanically even though it enforces nothing itself. The ticket's
`Risk: auto` line refers to the *research activity* (no code,
production or data change), which is accurate; the *merge* still needs
the operator's approval under trigger 7, plus one independent review
(`docs/**` is inside `one_review_include` and outside every
`one_review_exclude`, so one review covers it). A verdict note that
says `Risk: auto` for this head would misclassify; the honest line is
`Risk: human (7) — a docs/design/** path is on the trigger-7 list; the
doc changes no gate, actor or approval`.

Each mechanism delta above is its own follow-up ticket with its own
classification: M1/M2 touch `src/rollout.rs`, `src/update.rs`,
`src/cli/update.rs`, `src/daemon/**` → **human (1, 6, 7)** — identity,
fleet action, and a change to who may act. M5 touches the staging
workflow → **human (4, 6, 7)**.

## 10. Non-goals — what auto-update must never do

- Cross a schema migration unattended. `authorize_migration` already
  refuses without a lease+receipt; an auto tier must additionally
  *refuse to start* when `check` shows `migration: true` — the backup
  receipt existing is not permission to accept the one-way door.
- Install an unattested or un-promoted build. The attestation chain
  (sha on main → test green → artifact present → digest → manifest →
  `gh attestation verify` → version check) is non-negotiable; so is the
  `production-candidate` receipt binding promote approval to the bytes.
- Act without an attributable identity. No `--as` spoofing, no ambient
  operator proof inherited by a daemon-launched or detached process;
  the machine identity must be a first-class holder the lease, gate log
  and audit all name.
- Interrupt beyond the declared drain bound, or run outside the window
  because a run started inside it ran long — the window bounds the
  *start*, and a run that cannot finish inside it must not start.
- Restart the restarter. If the installer helper dies, the stale-marker
  un-wedge and `finish_restart` cover the fleet; resurrecting the
  installer itself is a human/external-supervisor job, and any external
  supervisor is a separate mechanism with its own admission proof —
  never a silent `cron` that inherits authority.
- Promote. T3 automates install only; the `production` environment
  approval stays the human's durable record.
- Fleet-wide actions. This policy covers the single production host;
  a canary/fleet tier is out of scope.

## Sources

- `src/update.rs` (pipeline phases, lease TTL, drain, health, rollback,
  `RunLock`, `UPDATE_STALE_SECS`, progress log, `finish_restart`)
- `src/cli/update.rs` (operator-only caller gate, `--as` requirement,
  pane refusal)
- `src/upgrade.rs` (attestation chain, `latest_green_main` = promoted
  dispatch only, `refuse_backwards`, trust labels)
- `src/rollout.rs` (lease model, `authorize_migration` one-way door,
  `begin_restart`/`recheck_restart`, grants, `delegate:` shape)
- `src/daemon.rs` (`update_drain` operator-connection gate,
  `marker_is_the_lease_holders`, drain adoption)
- `src/daemon/caller_rule.rs` (`update_drain` operator-only,
  `update_status` read)
- `src/peer.rs` (`operator_proof` ancestry checks)
- `src/ui/updates.rs` (detached helper spawn, board never runs the
  pipeline, `run_log_never_started`)
- `src/ui/operator.rs` (`POST /api/update` RouteClass::OperatorOnly)
- `.github/workflows/staging.yml` (select/stage/promote, environment
  gates, `require-full-gates`, promote = manual dispatch only)
- `scripts/auto-stage.py` (selector bounds, refusals, receipts)
- `config/production-baseline.json.example` (operator-pinned baseline)
- `docs/CI-DELIVERY.md` (gate posture, staging design, CAD-1102/770)
- `docs/roles/risk-classes.md` + `docs/roles/risk-paths.toml`
  (triggers 1,3,4,6,7; delegated class; scope approvals)
- `docs/design/staging-delegation.md` (CAD-1024 `delegate:` precedent)
