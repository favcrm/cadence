# Design contract: CAD-1024 staging delegation (operator-issued, scoped, expiring grant)

The rule being enforced: **only the operator, on positive proof, may create,
widen or revoke a delegation grant; and a delegate identity may exercise only
the ops its grant names, only on the staging state dir the grant is bound to,
only while the grant is live.** No actor may ever impersonate `operator:*`.

Precedent being extended, not invented: `rollout grant`/`revoke` (CAD-384,
`src/rollout.rs`, `src/daemon/approvals_rpc.rs`) already lets the operator let a
named *pane alias* claim the lease. CAD-1024's `delegate:<alias>` is the same
shape for a *non-pane shell* identity — the case CAD-384 cannot cover, because
the staging-refresh script runs from an agent-owned shell that must not need a
pane and must never pass `operator_proof`.

## Where each piece lives (existing code, reused — no new proof)

- **Operator proof.** `src/peer.rs::operator_proof(pid, uid, daemon_pid, panes,
  enrolled_root)` is the single positive proof: same uid, no registered pane or
  enrolled managed root on the `/proc` ancestry, no daemon-descendant hop, no
  ancestor carrying `CADENCE_ALIAS`/`CADENCE_RUNNER_ID`, no held pane pty,
  session leader on the ancestry. `src/rollout.rs::require_operator_proof`
  wraps it for the lease verbs (feeds `registered_panes` + `enrolled_roots` +
  `daemon_pid_for_proof` from read-only peeks; CAD-482's test seam answers
  first). `src/daemon/identity.rs::proven_operator`/`operator_connection` is the
  same proof for a *connection* (`SO_PEERCRED` pid) — `audit approve` reaches it
  through `rpc_approval_record`'s `operator_connection` gate. `staging delegate`
  and `staging revoke` reuse `operator_connection` exactly; **no new proof is
  introduced.**
- **Holder admission.** `src/rollout.rs::authorize_spawn_for` /
  `authorize_direct_run` / `begin_restart` / `claim_in` / `release` /
  `renew` / `record_backup` all funnel through `holder_block(conn, caller, now)`
  or `require_holder`, comparing `caller.identity` to the live lease's
  `holder`. `claim` already special-cases `caller.source == "as"` →
  `require_operator_proof` (an `operator:*` holder must be the operator).
  `caller_rule::Rule::Shutdown` + `granted_lease_holder` admit the lease-holding
  *pane* to `daemon stop`. The minimal change admitting `delegate:<alias>` is
  described under **Minimal change** below.
- **Caller identity derivation.** For RPC paths the caller is
  `connection_caller`/`slot_identity` — the nearest registered pane or enrolled
  managed endpoint on the peer's `/proc` ancestry, with `pid`+`pid_start`
  re-verified (CAD-280 positive proof). For the rollout CLI the identity is
  `resolve_caller`: `$CADENCE_ALIAS` (source `alias`) or `--as` (source `as`),
  and `reject_registered_alias` keeps `--as` from naming a live pane. The
  delegation path adds a third, narrower derivation — **contract item 4**.

## Invariants

- **I1 — operator-minted only.** `staging delegate` and `staging revoke` are
  admitted only by a connection that passes `operator_connection` (positive
  `operator_proof`; an agent-attributed or unproven connection is refused). A
  delegated agent, a detached (`setsid`) child, a forged `--as operator:*`
  field, and an env-scrubbed process all fail to create, extend or widen a
  grant.
- **I2 — allowlisted instance.** A grant can be created only for a state dir
  *registered as staging* in that dir's own store. Registration itself is
  operator-proofed. The production state dir, `~/pm`, port 3010 and the
  installed `~/.local/bin/cadence` are structurally ineligible — not by a
  denylist but because registration refuses them (see I5). A grant for an
  unregistered dir does not exist.
- **I3 — scoped and bound.** `delegate:<alias>` on `--as` is admitted only when
  (a) the calling process natively derives to `alias` (I4), (b) a live grant
  names `alias` **and the op being run** and **this state dir**, and (c) it is
  not expired/revoked. On any other state dir — including another staging dir —
  the same `--as delegate:<alias>` finds no grant and is refused.
- **I4 — native identity.** `delegate:<alias>` is admitted only when the
  caller's *process* identity proves `alias`. On the daemon RPC path that is
  `connection_caller` → `Who::Agent(alias)`. On the CLI lease path
  (`rollout claim`, `daemon start --as`) there is no daemon connection to lean
  on, so the CLI must derive `alias` positively: the process's own
  `CADENCE_ALIAS` must equal `alias` **and** the target state dir's `agents`
  table must register that alias with a live endpoint whose pid sits on this
  process's `/proc` ancestry (pane pid or enrolled managed root, `pid_start`
  re-verified) — i.e. the same derivation `operator_proof` runs, inverted. A
  forged `CADENCE_ALIAS` on an unregistered process, a scrubbed env, or an
  alias not registered **in that state dir** all fail. `delegate:` never names
  `operator:` and can never mint operator authority.
- **I5 — production can never qualify.** The staging registration and every
  grant check runs a hard allowlist: the state dir must resolve (via
  `sandbox::resolved`-style canonicalization) outside `client::default_state_dir()`,
  `home::local_state_dir()`, `issue::home_default_dir()` and `~/pm`; the board
  port recorded for it must be in 3110–3199; and the binary that will run is
  whatever `--state-dir` resolves, with the check applied to the dir, not the
  binary — the installed-binary exclusion is enforced by the fact that a grant
  is bound to a *state dir*, and the production state dir can never be
  registered. A `delegate:` claim against `~/.local/state/cadence` finds no
  grant because none can have been created.
- **I6 — lease records `delegate:<alias>`, never `operator:`.** When the
  delegate claims, `rollout_leases.holder` is the literal `delegate:<alias>`
  string (or `<alias>` with `holder_source` distinguishing it — contract item
  decides; see Minimal change). It never becomes `operator:*`, so `granted_lease_holder`,
  `require_holder` and the shutdown rule see a non-operator holder.
- **I7 — audit-readable.** Grant create, each use, revoke and expiry land on
  the `events` stream via `insert_event` (kinds `staging_delegate`,
  `staging_delegate_use`, `staging_revoke`) carrying `delegate:<alias>`, the
  ops, the expiry and the state-dir identity — readable by `cadence audit` /
  `agent events` exactly as `rollout_grant`/`rollout_claim` are today.

## Minimal change to admit `delegate:<alias>` (the seam, stated precisely)

`resolve_caller_with` gains a third identity shape. When `explicit_as` parses
as `delegate:<alias>`:

1. `CADENCE_ALIAS` (ambient env) must equal `<alias>` — an agent shell that
   lost or forged it fails (I4).
2. The caller source is a new `source: "delegate"`, so it is **not**
   `source == "as"` and does **not** take the `require_operator_proof` branch
   in `claim` — while also being refused by `reject_registered_alias` only if
   `<alias>` is registered (it is, on a real delegate; that check stays for
   `as` source only).
3. Before `claim_in`/`authorize_spawn_for` run, a `require_delegate_grant(
   state_dir, alias, op)` check reads a `staging_grants` table (new, beside
   `rollout_grants`): live row for `(alias, state_dir, op)` within TTL, else
   `Err(rejected)`. `op` is the verb name (`rollout_claim`, `daemon_start`,
   `daemon_stop`, `ui_start`, `ui_stop`, `app_upgrade`).
4. `holder` on the lease is recorded as `delegate:<alias>` so it can never be
   confused with an operator holder (I6).

On the daemon RPC side (`shutdown`, and any `staging_*` RPC), `connection_caller`
already yields `Who::Agent(alias)`; the change is that `Rule::Shutdown`'s
`lease_holder` check accepts a lease whose holder is `delegate:<alias>` when
the caller is `Who::Agent(alias)` and a live grant covers `daemon_stop` — a
one-line extension of `granted_lease_holder`'s lookup shape, not a new gate.

`holder_block`, `require_holder`, `force_holder_refusal`, `preview_force_refusal`
and the take-over path are **unchanged** — they compare holder strings; a
`delegate:<alias>` holder is just another non-operator holder to them.

## Grant store

- **Location:** `staging_grants` table in the state dir's own
  `cadence.sqlite3`, created by `ensure_lease_tables`-adjacent migration.
  Binding the grant to the *file* the ops act on is what enforces I3 for free:
  another state dir's DB has no such row.
- **Columns:** `id`, `alias`, `ops` (JSON array of op names), `state_dir`
  (canonical path, recorded at create), `granted_by`, `granted_at`,
  `expires_at` (NOT NULL — a grant always expires; default ≤ 7d, reusing
  `parse_ttl` + `MAX_TTL`), `revoked_at`, `revoked_by`, `last_used_at`.
- **Staging registration:** `staging_register` writes a marker row
  (`staging_instances`: `state_dir` canonical, `board_port` 3110–3199,
  `registered_by`, `registered_at`) into the same DB after the I5 allowlist
  passes. `staging delegate` refuses without it.
- **TTL/revoke:** `expires_at <= now` or `revoked_at IS NOT NULL` ends the
  grant; checks run inside the same `immediate` transaction as the use, so an
  in-flight second call after a revoke sees the revoked row (linearized).
- **Audit:** every transition `insert_event`s onto `Store::DAEMON_STREAM`;
  `cadence audit` and `agent events` read them without a new reader.

## Failure modes

- [ ] **Crash/SIGKILL between grant write and event.** Both run in one
      `immediate` transaction — a torn grant cannot exist without its event,
      and a failed grant leaves no row. Recovery re-reads the DB; nothing is
      replayed.
- [ ] **Loaded host.** A `daemon start` health precheck timing out is already
      handled (`already_running` vs spawned-child bookkeeping); the grant
      check is a read-only peek before the spawn decision, so a slow host only
      delays, never widens, admission. A delegate's `--takeover` on a live
      lease is refused exactly as for any holder.
- [ ] **Wrong caller.** I1/I4: agent-attributed RPC → refused by
      `operator_connection`; unproven/detached → refused by `operator_proof`;
      `delegate:` from a process whose `CADENCE_ALIAS` doesn't match, or whose
      ancestry holds no live endpoint of `alias` **registered in that state
      dir**, is refused.
- [ ] **Concurrent callers.** Two delegates on the same dir: the lease's
      `rollout_lease_one_active` partial unique index resolves to one holder,
      as today. Grant create vs use: `immediate` transaction linearizes; a
      revoked-then-used race reads the post-revoke row.
- [ ] **Forked/`setsid` child.** For the RPC path, `operator_proof`'s
      session-leader check already refuses orphans. For the CLI path, the I4
      ancestry check is the guard: a detached child of the delegate's pane keeps
      the pane on its ancestry → still derives `alias` → **still admitted**
      (deliberate: it is that agent's own child, holding that agent's grant —
      the grant is the authorization, the ancestry only binds identity). What
      is refused: a detached child that *left* the ancestry is `unproven` and
      derives no alias; a *different* agent's child never derives `alias`.
- [ ] **Relay/HTTP paths.** v1 exposes no board/HTTP route for delegate or
      revoke (operator CLI + daemon RPC only). If a board route is added, it
      sits behind the same `operator_connection` gate the other operator write
      routes use (BOARD.md's 403 `operator_only`/`operator_proof` pattern) —
      never weaker.
- [ ] **Forged field.** `--as delegate:<alias>` where `<alias>` ≠ env or
      unregistered → refused (I4). A request field `holder`/`by`/`agent`
      naming the operator is refused by `reject_operator_fields` /
      `IDENTITY_FIELDS`. `--as operator:*` still takes
      `require_operator_proof` and is unchanged.
- [ ] **Partial write.** Grant row + event commit atomically;
      `staging_grants` rows are insert-only with `revoked_at` marking, same
      pattern as `rollout_grants`.
- [ ] **Clock/TTL edges.** `expires_at` is epoch seconds vs `unix_now()`;
      boundary expiry is refused (`<= now`). A delegate mid-`daemon restart`
      whose grant expires still holds the lease until the lease's own TTL —
      the lease, once held, governs; the grant governs *admission to claim*,
      matching how `rollout_grant` + lease behave today.

## Adversarial tests (each fails without its guard)

| Test | Proves | Guard | Fails without the guard because |
|---|---|---|---|
| `delegate_refuses_agent_connection` | I1 | `operator_connection` on `staging_delegate` RPC | an agent pane's RPC writes a grant |
| `delegate_refuses_setsid_unproven` | I1 | `operator_proof` session/ancestry checks | a detached child records a grant |
| `delegate_refuses_forged_operator_as` | I1 | `reject_operator_fields` + `operator_connection` | `--as operator:x` on the delegate verb mints a grant |
| `delegate_refuses_scrubbed_env` | I1 | `operator_proof` env-hop check | env-scrubbed agent child still creates a grant |
| `delegate_refuses_unregistered_dir` | I2/I5 | `staging_instances` allowlist lookup | a grant is created for the production/default state dir |
| `delegate_use_binds_state_dir` | I3 | `staging_grants.state_dir == this dir` | the grant works on a *second* staging dir |
| `delegate_use_binds_ops` | I3 | `ops` allowlist contains the verb | `delegate:` runs `ui tailscale` / a non-granted op |
| `delegate_expired_refuses` | I3 | `expires_at <= now` in-tx check | a stale grant still admits a claim |
| `delegate_revoked_refuses` | I3 | `revoked_at IS NULL` in-tx check | a revoked grant admits a second call |
| `delegate_two_delegates_one_holder` | I3 | `rollout_lease_one_active` unique index | two delegates both hold the lease |
| `delegate_other_alias_refused` | I4 | `CADENCE_ALIAS == alias` + ancestry endpoint | agent B uses agent A's grant |
| `delegate_forged_env_refused` | I4 | registration + ancestry check | `CADENCE_ALIAS=B` on an unregistered process claims as `delegate:B` |
| `delegate_holder_never_operator` | I6 | `holder = delegate:<alias>` | lease holder reads `operator:*`, bypassing grant checks |
| `delegate_audit_reads` | I7 | `insert_event` on each transition | grant/use/revoke are invisible to `cadence audit` |
| `operator_mode_unchanged` | regression | `source=="as"` → `require_operator_proof` | the swap script's operator path changes behaviour |
| `delegate_board_route_strict` | relay | `operator_connection` on any future route | a board peer weaker than the RPC mints a grant |

Mutation proof: each test is written first and re-run with the single guard it
names removed (grant-existence check, TTL check, state-dir bind, ops allowlist,
alias/ancestry check), shown red, then restored.

## PR split (each ≤ ~300 changed lines)

1. **PR-1 (this PR):** `docs/design/staging-delegation.md` only.
2. **PR-2:** `staging_grants` schema + `staging delegate`/`revoke`/`delegations`
   CLI+RPC under `operator_connection`; registration verb + I5 allowlist;
   events. No `delegate:` caller shape yet — grants exist but admit nothing.
3. **PR-3:** `resolve_caller` `delegate:` shape + `require_delegate_grant` +
   `holder = delegate:<alias>` + `shutdown`'s `lease_holder` extension; the
   adversarial use tests.
4. **PR-4:** delegate mode in the staging-refresh path (`--as
   delegate:<alias>` plumbed through `daemon start`/`daemon stop`/`ui start`/
   `ui stop`/`rollout claim` for the granted dir) + regression tests that
   operator mode is unchanged.

## Out of scope

- Tailnet mapping changes and any op outside the v1 ops allowlist — stay
  operator-only.
- `cadence staging refresh` as a single supported command — a later ticket;
  v1 admits the individual ops the script already runs.
- Broadening `operator_proof` to admit enrolled endpoints as operator —
  explicitly rejected; the grant is the authority, not a relaxed proof.
- App-only upgrades via the installation API — already out of scope per the
  ticket.
