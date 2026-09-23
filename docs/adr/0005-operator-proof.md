# 0005 — Operator by positive proof: a factor the operator holds, never an absence

- Status: **proposed**. Not accepted. No code may be written against it
  until the operator accepts it (CAD-280 acceptance item 1). Risk class
  **human**: trigger 1 (a rules and trust boundary: who the operator is).
- Date: 2026-09-23
- Author: CAD-280 design lane (`cad280-dev`, dispatched by `op-pm`)
- Deciders: the operator, who answers every question in §9. After that an
  independent security reviewer checks the design, and only then may an
  implementation lane start.
- Issues: CAD-280 (this), CAD-288 (merged into CAD-280), CAD-276 and
  CAD-263 (the accepted residuals, §7), CAD-313 / ADR 0004 (PR #176,
  the operator's web session), CAD-335 (managed agents write as the
  operator), CAD-149 / CAD-304 (PR #187, the caller rule on `agent
  set/remove/gc`), CAD-337 (PR #184, `model_defaults_set`), CAD-217
  (approval records), CAD-292 (`rollout release --force`), CAD-336 (#190,
  the tailnet proxy proof), CAD-315 (macOS, which has no `/proc`)
- **Relationship to ADR 0004.** ADR 0004 (still proposed, PR #176)
  designs the *web transport*: the login link, the cookie, the session
  store in the daemon, and the route table. It leaves open the *factor*
  that proves the operator. In ADR 0004 phase 1 that factor is
  `operator_proof` plus a 0600 file, and ADR 0004 hands the gap back to
  CAD-280 (its §4 phase 2, §5.7, §8). This ADR decides that factor, and
  the one rule every front door uses: the daemon socket, the board, and
  the audit. If both ADRs are accepted, this ADR replaces ADR 0004's mint
  factor (its §5.1 and §5.7). The rest of ADR 0004 stands.
- **Code citations are pinned to `77529d8ee2932618ce7008893831db0506a41274`**
  (origin/main, 2026-09-23). Line numbers move, so find code by the quoted
  symbol.

## 1. Context

### 1.1 The rule today: the operator is whoever is left over

Cadence has no positive notion of "the operator". Every front door
decides authority by elimination. If a caller cannot be tied to an agent,
it is treated as the operator. The rule takes three forms:

| Front door | The leftover rule | Where |
|---|---|---|
| Board writes | A TCP peer tied to no registered pane or managed endpoint writes as `operator (ui)` | `src/ui.rs` `write_caller`, the `Ok(None)` arm (CAD-335 has the evidence) |
| Tracker CLI writes | No `--by` and no `CADENCE_ALIAS` means `operator` | `src/issue/write.rs` `actor_who` |
| Daemon verbs gated by `operator_proof` | The caller is the daemon's uid, its ancestry has no pane, enrolled endpoint, daemon hop, `CADENCE_ALIAS` or `CADENCE_RUNNER_ID`, it holds no pane's pty, and its session leader is on its own ancestry | `src/peer.rs` `operator_proof`, `src/daemon.rs` `proven_operator`, `approval_operator` |
| Daemon verbs with no caller check | A request field (`by`) names the actor, and it defaults to `"operator"` | `src/daemon.rs`, for example `optional_str(params, "by").unwrap_or("operator")` |

`operator_proof` is the strongest of the three. It is still a list of
things the caller does *not* have. Every check is a shape that an
agent's process could have. No check is something that only the operator
could have. So each new detach shape needs one more check, and there is
always one more shape.

§1.2 has the full inventory of what the leftover rule authorizes.

### 1.2 What "operator" authorizes today

This inventory is taken at `77529d8`. It does not include open PRs
#184 (CAD-337) and #187 (CAD-149/304), which add caller checks to
`model_defaults_set` and to `agent set/remove/gc`.

**Class 1 — `operator_proof` gated.** These are positive in name only
(§1.3).

| Surface | Gate |
|---|---|
| `slot_reconcile` | `rpc_slot_reconcile`: field refusal, then `slot_identity` is `None`, then `proven_operator` |
| `approval_record`, `approval_revoke` (CAD-217) | `approval_operator`. The record is stamped `recorded_via: operator-connection`. |
| `plan_approve`, `plan_reject` (CAD-360) | `approval_operator`. The actor is a hard-coded `"operator"`. |
| `plan_propose`, the no-agent branch | `operator_evidence`. The actor becomes `"operator"`. |
| `slot_launch` and `slot_runner`, the no-agent branch | `launch_requester` or `proven_operator`, on lane `(operator)` |
| `rollout release --force` (CLI; works with the daemon stopped, CAD-292) | `rollout.rs` `require_operator_proof`, plus a caller-chosen `--as` name |

**Class 2 — connection-derived, with operator as the default.** No agent
identity means the operator.

| Surface | Where the default happens |
|---|---|
| Every board write route: issue create, update, links, refs, comments and artifacts; monitor alert ack; model defaults; operator chat | `src/ui.rs` `write_caller`, where `Ok(None)` becomes `WriteCaller::Operator("operator (ui)")`. It also returns `Ok(None)` when no agent roots exist, and when a non-loopback peer has no local socket. |
| `thread_send` (operator chat) | `caller_identity` is `NoAgentIdentity`, then `source: "operator"`. The code comment calls this "a default, not positive proof". |
| `agent_send`, `agent_ask` | `thread_sender`: `NoAgentIdentity` becomes `Sender::Operator` |
| `agent_answer` | `unmatched_caller`: `operator` only with "positive terminal evidence" (a non-pane pty). That is still a shape, not a factor. |

**Class 3 — request-field-chosen.** The daemon never checks who is
calling.

| Surface | How the actor is chosen |
|---|---|
| `message_reconcile`, `message_cancel`, `agent_unfence`, `task_dispatch`, `task_accept`, `task_sha`, `task_fail`, `task_cancel`, `monitor_alert_ack`, `job_cancel`, `job_close`, `agent_ready` | `optional_str(params, "by").unwrap_or("operator")` |
| `task_reopen`, `monitor_stop`, `monitor_dispatch` | Treated as the operator unless the request carries a `pane` field |
| `agent_register`, `agent_set`, `agent_remove`, `agent_gc`, `agent_resume`, `agent_stop`, `model_defaults_set`, `job_new`, `task_new`, `shutdown` | No caller gate at all (on this base) |

**Class 4 — CLI-side, with no daemon involved.** Tracker writes are
direct file writes by the `cadence` binary. They stamp `--by`, else
`CADENCE_ALIAS`, else `"operator"` (`src/issue/write.rs` `actor_who`,
and the author fallbacks in `issue/report.rs` and
`issue/task_report.rs`). No daemon rule can govern these until they are
routed through the daemon (§6.1 step 3).

**Already correct:** the memory verbs (`memory_actor` refuses
`NoAgentIdentity`) and the board's memory accept and reject routes
(always refused).

### 1.3 How the rule is defeated (reproduced, not hypothetical)

Every shape below has been run from inside an agent pane and passed as
the operator:

| Shape | Passes | Evidence |
|---|---|---|
| `env -u CADENCE_ALIAS setsid -f cadence build-slot reconcile …` (it becomes its own session leader and is reparented to init) | `operator_proof` | CAD-276 independent review of PR #146. The operator accepted it as a residual on 2026-09-23. |
| `tmux -L cadence-<hash> new-window -d 'exec env -u CADENCE_ALIAS cadence …'` (cadence's own tmux server, whose label is predictable) | `operator_proof` | qa-2's review of PR #146, filed as CAD-288, now merged into CAD-280 |
| An agent-started tmux or screen server, `systemd-run --user`, `ssh localhost`, cron, `at` | `operator_proof` (the same class as above) | the residual text in `peer.rs` `operator_proof`'s doc comment |
| `setsid -f env -i PATH=$PATH sh -c 'cadence agent set w2 approval_policy=never --next-launch' </dev/null` | the caller rule on `agent set/remove/gc`. It lands and is audited `by:operator`. | qa-pr187 F1 (PR #187), comment on CAD-280, 2026-09-23T14:31Z |
| `curl` to the board from a `setsid` child, a managed agent's tool shell, or any process that has left every pane's ancestry | `write_caller` gives `operator (ui)` | CAD-335. ADR 0004 F1. |
| **The test suite itself** | `operator_rpc` in `tests/integration.rs` reaches the operator verbs with `setsid -f` plus `env_clear` plus null stdio, because that is how an operator shell looks | `tests/integration.rs` `operator_rpc` (its doc comment says so) |

The last row matters most. Cadence's own harness cannot be the operator
except by using the escape an agent would use. So today's tests cannot
tell the operator from an agent, because the two look the same.

### 1.4 Host facts that bound any design (measured 2026-09-23)

| Fact | How it was measured |
|---|---|
| The agents run as the operator's uid (`ubuntu`, uid 1000) | `id` in this lane's pane |
| That uid is in `sudo`, and `sudo -n true` succeeds (NOPASSWD) | qa-1, CAD-336 review r3 (`/var/www/agent-notes/…-cad-336-tailnet-proxy-proof-r3-verdict.md`, finding B) |
| `kernel.yama.ptrace_scope = 1`: a same-uid process cannot read the memory of a process it did not start, unless it is root | `/proc/sys/kernel/yama/ptrace_scope` |
| Landlock is active. Unprivileged user namespaces are restricted by AppArmor. | `/sys/kernel/security/lsm`, `/proc/sys/kernel/apparmor_restrict_unprivileged_userns = 1` |
| The state dir, `cadence.sqlite3`, `slots.json`, the release directory and the `~/.local/bin/cadence` symlink are all owned by uid 1000 | `ls -l` |
| An agent pane can list, and so drive, the sessions on the user's default tmux server (for example `aos-pm`) | `tmux ls` from this lane's pane (read only; nothing was sent) |
| `ssh-keygen` (OpenSSH 9.6) is installed. It can sign and verify with `-Y sign` / `-Y verify`, including FIDO2 `-sk` keys. | `ssh -V` |
| The §5 flow works with the stock tool: an `allowed_signers` line `operator namespaces="cadence-*" <key>` verifies a `-n cadence-operator` signature (exit 0). A tampered statement fails (exit 255, "incorrect signature"), and so does a wrong namespace (exit 255, "namespace does not match"). | A throwaway key in this lane's scratchpad, deleted afterwards |
| The only crypto crate in `Cargo.toml` is `sha2` | `Cargo.toml` `[dependencies]` |

### 1.5 Threat model

Four adversary classes. The rest of this ADR refers to them by these
names.

| Class | What it does | Example |
|---|---|---|
| **A0 — honest mistake** | It runs the wrong cadence verb, or the right one on the wrong target | A worker runs `cadence agent remove` on a peer |
| **A1 — API-level evasion** | It runs cadence verbs, or `curl`s the board, from whatever process shape passes the gate. It never goes around the API. | Every row of §1.3 |
| **A2 — same-uid bypass of the API** | It uses the uid directly: it reads any file the uid can read, writes `cadence.sqlite3` or the tracker directly, replaces the binary or the symlink, restarts the daemon with a patched build, types into another tmux pane (`send-keys`), or runs `gh pr merge` with the shared token | ADR 0001 §1.4. `docs/AUDIT.md`, "Approvals are operator claims", gap 2. |
| **A3 — root** | `sudo` | Every agent on this host today (§1.4) |

What can and cannot be true:

- **Against A3, no in-host mechanism holds.** Root can read any key,
  patch the daemon, and forge any row. While the agent uid has NOPASSWD
  sudo, every in-host proof in this ADR is a guardrail, not a boundary.
  That is a fact about the host, not about the design. §9 Q1 asks the
  operator to decide it.
- **Against A2, only a different OS user holds** (Option D), and then only
  if that user cannot sudo. Under one uid, A2 can always forge state that
  lives on the host. The most a single-uid design can do against A2 is
  make each forgery **deliberate, multi-step and detectable**, and make
  the evidence that matters **verifiable off the host** (Option C's
  signed records).
- **Against A0 and A1, a positive proof holds completely**, provided the
  factor is something no process shape carries and no single file read
  yields. That is CAD-280 acceptance item 2's target: "a same-uid agent
  process, in any detach shape, cannot obtain operator attribution". It
  is achievable against A1. It is not achievable against A2 or A3 on a
  single uid, and this ADR says so rather than claiming it.

## 2. What "done" looks like

- **P1 — no authority from absence.** No front door grants operator
  authority or attribution because a caller *lacks* something (a pane
  tie, an env alias, a TTY, a pty). An unattributable local caller gets
  public reads only. Every write is refused, and the refusal names the
  remedy (§5.4). The caller gets **neither operator nor agent
  authority**, which is the title of CAD-280.
- **P2 — one proof, one function.** The daemon holds one verifier
  (`operator_grant`). `slot_reconcile`, the approval verbs, plan
  decisions, `agent set/remove/gc`, `agent_register`,
  `model_defaults_set`, `rollout release --force`, and every board
  operator route (via ADR 0004's session, which the daemon mints) all
  call it. No verb keeps a private operator rule.
- **P3 — independent of process shape.** Whether a caller is the
  operator never depends on its ancestry, session, env or stdio, so no
  detach shape can change the answer. Process signals stay only as a
  *narrowing* layer: they can refuse, never grant.
- **P4 — the audit can tell proof from claim.** `docs/AUDIT.md` marks
  every approval `operator-claimed · unverified until CAD-280`. After
  this ADR, an approval is either **verified** (by a check that A2 cannot
  satisfy on this host) or reported as a claim. It is never silently
  upgraded.
- **P5 — honest residuals, pinned by tests.** Every path that still
  gives operator without the operator is listed in §8. Where a test can
  express it, a test pins it, so that a fix flips it on purpose.
- **P6 — the operator keeps working.** The cost is bounded: one unlock
  per terminal per working session for routine acts, and one
  confirmation per human-class act. Break-glass exists (§6.4).
  Delegation to a PM session is explicit (§5.5), never a side effect.
- **P7 — portable.** The proof works without `/proc`, on macOS
  (CAD-315), and in CI.

## 3. Options

Each option is judged on six things: what it proves; how it fares
against A1, A2 and A3; what an unproven caller gets; what it costs; how
board writes, `slot_reconcile` and `approval_record` migrate; and how it
is tested.

### Option A — Keep refining process heuristics (today's `operator_proof`)

- **Proves:** that a caller does not have any of the agent shapes the
  code knows about.
- **A1:** it fails. Every row of §1.3 passes. Each fix (the session-leader
  check, the `CADENCE_RUNNER_ID` check, the subreaper from CAD-308, the
  CAD-288 tmux-label check) closes one shape and leaves the class. An
  agent can always start a long-lived process that the daemon did not
  launch.
- **Unproven caller gets:** operator. That is the defect.
- **Cost:** none now, and an arms race forever. It is also empty on macOS
  (no `/proc`) and for HTTP (the operator's browser holds no terminal;
  ADR 0004 option F).
- **Verdict: rejected as the proof.** It is kept as a narrowing layer
  (P3). It still catches A0 and the naive A1 cheaply, with a precise
  refusal message.

### Option B — A host-stored operator secret (possession of a 0600 file)

This is ADR 0004 phase 1, applied to the CLI as well as the board. A
32-byte secret lives at `<state>/operator/secret`, mode 0600. Operator
verbs present it over the socket. The board login link is minted only by
a caller that presents it.

- **Proves:** that the caller could read one file owned by the uid.
- **A1:** it holds only against a caller that never reads the file. But
  `cat ~/.local/state/cadence/operator/secret` is a single ordinary tool
  call. Agents list the state directory routinely (this lane did, while
  gathering §1.4). A prompt-injected agent needs one line. The file
  turns ambient authority into *one-step* authority.
- **A2 / A3:** no effect. The mitigation in ADR 0004 is detection
  (inotify `IN_OPEN`, no pid, no auto-rotate), and then phase-2
  confinement.
- **Unproven caller gets:** refused, once the leftover rule is flipped (P1).
- **Cost:** low. There is no typing, and the file is created at `setup`.
- **Migration:** board as in ADR 0004. `slot_reconcile` and
  `approval_record` gain a `secret` parameter. The harness writes a test
  secret.
- **Tests:** a caller without the file is refused, a caller with it is
  accepted, whatever its shape.
- **Verdict: not sufficient for CAD-280 acceptance item 2** on a single
  uid. Any detach shape plus `cat` passes. ADR 0004 says so itself (its
  §4 and §5.1: "The 0600 file does not stop an agent"). It is kept only
  where it has real value: when agents run under another uid (Option D),
  the same file becomes a real boundary for free.

### Option C — Proposed: an operator signing key, off the agents' reach, used by challenge-response and to sign records

The operator is **whoever can produce a signature with a registered
operator key**. The key never exists in plaintext on the host where the
agents run. The operator chooses one of two kinds of key (§9 Q2):

| Kind of key | What producing a signature proves | Where the private key lives |
|---|---|---|
| **FIDO2 hardware key** (`ssh-keygen -t ed25519-sk`, touch required, optionally a PIN with `verify-required`) | **A human was present** and touched the key, for this challenge | The private key is inside the token. On the host there is only a key *handle*, which is useless without the token. |
| **Passphrase-encrypted ed25519 key** (OpenSSH format, bcrypt-pbkdf) | **Someone who knows the passphrase** typed it for this challenge | The host holds an encrypted file. Reading it gives an offline brute-force target, not a key. |

The mechanism is OpenSSH's own signature format, which git's ssh signing
also uses:

- **Sign:** `ssh-keygen -Y sign -n <namespace> -f <key>`. This prompts
  on `/dev/tty` for the passphrase or the touch.
- **Verify:** `ssh-keygen -Y verify -n <namespace> -f
  <allowed_signers> -I operator`.

Cadence shells out to `ssh-keygen` on both sides. That means **no new
crate** (§1.4). It works on Linux and macOS alike, and anyone holding
`allowed_signers` can verify, including CI.

The key is used in two ways:

1. **Challenge-response grants (live authority).** The CLI asks the daemon
   for a challenge. The daemon returns a fresh nonce plus the exact
   statement to be signed. The CLI shows the statement, signs it, and
   returns it. The daemon verifies it against its registered
   `allowed_signers` and issues a **grant** (§5.2). The statement names
   the daemon instance, the nonce, the expiry, and **what is being
   authorized**: either one action, or a terminal unlock.
2. **Signed records (durable evidence).** A human-class decision, such as
   an approval of an exact head, a plan decision, or a force-release, is
   stored **with its signature** over a canonical record (repo, PR, full
   head SHA, class, decision, time, nonce). `cadence audit` verifies the
   signature. So can CI, with `allowed_signers` committed to the repo.
   **Writing that row directly into `cadence.sqlite3` (A2) cannot forge
   the signature.** This is what lets the audit move from
   `operator-claimed` to `verified` (P4).

- **A1:** it holds for every detach shape. No process shape produces a
  signature. `setsid -f`, `env -i`, its own tmux server,
  `systemd-run --user` and a scrubbed stdio all produce the same thing:
  a caller without a grant.
- **A2:** mixed, and stated precisely:
  - **Signed records: they hold.** A2 can delete or roll back rows, but
    it cannot forge one. The CI verifier sees any forgery. A deletion
    shows up as a missing approval, which the audit already reports.
  - **A hardware key: holds for each signature.** A2 cannot touch the
    token. It *can* trigger a signing prompt at a moment when the
    operator expects one (a confused touch). §5.3 makes the prompt show
    the statement.
  - **A passphrase key: holds until A2 captures the passphrase.** It can
    do that by replacing the `cadence` or `ssh-keygen` binary on `PATH`
    with a trojan (the symlink is uid-owned, §1.4), by brute-forcing the
    encrypted file offline, or by typing into an unlocked operator
    terminal with tmux `send-keys` (§8 R2). Each of these is a
    deliberate, multi-step act of A2, and none of them is a process
    shape.
  - **A routine terminal unlock (§5.2, kind `unlock`): it lends
    authority** to anything that can run inside that kernel session,
    including tmux `send-keys` into it. That is the price of not asking
    for a touch every time. It is bounded by the TTL and by the rule that
    human-class acts always need their own signature.
- **A3:** it fails for live authority (root patches the daemon). **Signed
  records still hold** if they are verified off the host (in CI), and a
  hardware key's touch still cannot be faked. This is the only option
  whose evidence survives A3.
- **Unproven caller gets:** public reads. Writes are refused, and the
  refusal names `cadence operator unlock` (§5.4).
- **Cost:**
  - One-time: a key (a FIDO2 token costs about 25–60 USD, or use a
    passphrase key), plus `cadence operator enroll`.
  - Per working session: one `cadence operator unlock` per terminal.
  - Per human-class act: one touch or one passphrase.
  - Recovery needs a second registered key (§6.4).
  - The operator's PM session needs explicit delegation (§5.5).
- **Migration:** §6. **Tests:** §10.
- **Verdict: proposed.** It is the only option that closes A1 by
  construction, works for HTTP and CLI alike, works on macOS, adds no
  crate, and gives the audit evidence that survives A2.

### Option D — A separate OS user for agents, with no sudo

The agents run as `cadence-agent`. The operator stays `ubuntu`.

- **Proves:** a kernel fact. `SO_PEERCRED` uid 1000 is the operator, and
  anything else is not.
- **A1 and A2:** they hold. A different uid cannot read 0600 files, cannot
  signal or ptrace the operator's processes, and cannot reach a 0700 tmux
  socket. **This is the only option that defends A2 for live authority.**
- **A3:** it holds only if `cadence-agent` has no sudo, and has no write
  access to anything root runs. If the agent uid can sudo, D is worth
  nothing.
- **Unproven caller gets:** by uid. The agent uid gets agent authority by
  pane or enrollment, as today. Any other uid is refused.
- **Cost:** high, and ADR 0004 option D lists it in full. Install needs
  root. Provider credentials, `gh` auth and ssh keys live in the
  operator's home, so they must be split or shared. Worktrees need group
  ownership and `safe.directory`. tmux and the pty adapter assume one
  user. On this host, it also means **taking NOPASSWD sudo away from the
  agents**, which is a posture change the operator must decide (§9 Q1).
- **Verdict: supported as a hardened deployment, not the default.**
  Nothing in Option C assumes one uid. Under D, Option C's key and grants
  gain a kernel boundary around them for free.

### Other ideas considered, briefly

- **A "declared operator process" without a factor.** The operator runs
  `cadence operator declare` in a shell, and the daemon trusts that
  shell's session. An agent can run the same command, so a declaration
  without a factor is Option A again. It survives as the *binding* half
  of Option C's `unlock` grant: declaring a session costs a signature.
- **A tailnet login as the operator.** ADR 0004 option E already rejects
  this. It stays attribution only, never authority.
- **Refuse all unattributable writes and add nothing (ADR 0004 option
  B).** This locks the operator out. It is P1, and it is a component of
  Option C, not a decision on its own.

### Comparison

| | A heuristics | B 0600 secret | **C operator key** | D separate uid |
|---|---|---|---|---|
| A1: any detach shape | fails | fails (`cat`) | **holds** | holds |
| A2: live authority | fails | fails | holds per signature; the unlock TTL lends | **holds** |
| A2: forged approval rows | fails | fails | **holds** (verified signature) | holds |
| A3: root | fails | fails | records hold off host; touch holds | holds only without agent sudo |
| Works for HTTP / the board | no | yes | yes (it mints ADR 0004's link) | yes |
| macOS, no `/proc` | no | yes | **yes** | yes |
| New crates | none | none | **none** (`ssh-keygen`) | none |
| Operator cost | none | none | a key, an unlock, a touch | root install, credential split |

## 4. Decision (proposed)

Adopt **Option C**, with P1 flipped at the same time, **Option A demoted
to a narrowing layer**, and **Option D recognised as the hardened
deployment**.

1. **The rule.** Operator authority is a **grant** that the daemon issued
   after verifying a signature by a registered operator key. Nothing
   else is operator: not being tied to a pane, not a TTY, not a tailnet
   login, not a file on disk.
2. **An unattributable local caller gets public reads only.** Every write
   is refused, whether it is operator-only or would be agent-allowed for
   an attributed agent. The refusal names the remedy.
3. **One verifier.** The daemon's `operator_grant(conn, act)` is the only
   operator check. The board calls it through ADR 0004's session, and a
   session is minted only against a signed `login` statement.
4. **Human-class acts are signed per act, and the signature is stored.**
   These are approval record and revoke, plan approve and reject, and
   rollout force-release. That signature is the audit's evidence.
5. **Routine operator acts accept a terminal unlock.** These are
   `slot_reconcile`, `agent set/remove/gc/register`, model defaults, and
   board operator writes through a session. The unlock is a signed,
   time-boxed grant bound to one kernel session (§5.2).
6. **Delegation is explicit.** A PM session never *is* the operator. It
   can hold a scoped, time-boxed, signed delegation (§5.5).

What this buys against CAD-280's acceptance:

- **Item 2 holds against A1**, for every detach shape, by construction.
- Against A2 it becomes a deliberate act, and a detected one where
  detection is possible.
- Against A2 and A3, human-class records stay verifiable.

## 5. Design sketch

This section gives enough for an implementation lane to estimate the
work and a reviewer to attack the design. Field names are illustrative.

### 5.1 Enrolment

- **Enrol:** `cadence operator enroll --key <path-to-.pub>` appends
  `operator namespaces="cadence-*" <key>` to
  `<state>/operator/allowed_signers` (mode 0600).
  - **The first enrolment** needs a caller that passes today's
    `operator_proof` **and** a signature by the key being enrolled
    (proof of possession).
  - **Every later enrolment or removal** needs a signature by an
    already-enrolled key. From then on, `operator_proof` alone never
    changes the key set.
- **Detection:** the daemon keeps `allowed_signers` in memory. It
  refuses to use a file that changed on disk without going through the
  verb, and raises `operator_signers_changed` on the Overview "needs me"
  rail. A2 can still restart the daemon to reload a forged file. That
  restart is itself a daemon event, and the daemon logs the key
  fingerprint set at every start.
- **The repo copy:** for CI verification, `allowed_signers` is also
  committed. `.cadence/operator_signers` is a public key, not a secret.
  Changing it is a risk-class `human` change (trigger 1).

### 5.2 Grants (challenge-response)

`cadence operator unlock [--ttl 8h]`, and the implicit per-act prompt,
follow three steps:

1. **CLI → daemon: `operator_challenge {kind, act?}`.** The daemon
   returns the statement to sign:

   ```
   cadence-operator-v1
   daemon: <daemon-instance id>
   nonce: <32 random bytes, base64url>
   kind: unlock | act | login | delegate
   act: <canonical JSON of the one action>       # for act
   session: sid=<sid> leader_start=<ticks> tty=<dev>  # for unlock
   delegate: <alias> verbs=<…> until=<time>      # for delegate
   expires: <time, at most 120 s after issue>
   ```

2. **CLI:** it prints the statement in plain words ("Unlock operator
   authority for this terminal until 18:30", or "Approve PR #176 at
   3c1f…, class human"). Then it runs
   `ssh-keygen -Y sign -n cadence-operator` on the operator's key.

3. **CLI → daemon: `operator_prove {nonce, signature}`.** The daemon
   verifies with `ssh-keygen -Y verify -n cadence-operator`, the
   in-memory `allowed_signers`, and `-I operator`. It checks that the
   nonce is single-use and unexpired, and that the daemon id matches.
   Then it issues the grant:
   - **`act`:** consumed by exactly the action the statement names,
     within the same request. The canonical record and the signature are
     stored together.
   - **`unlock`:** the daemon records `(uid, sid, session-leader
     starttime, ctty)`. A later caller qualifies when its own `/proc`
     stat shows that sid, and the leader's starttime and the ctty match,
     and the TTL has not lapsed.

     On Linux these are **kernel facts no unprivileged process can
     choose**. A process can leave a session (`setsid`) but cannot join
     an existing one, and it cannot take a controlling tty that another
     session owns (`TIOCSCTTY` needs `CAP_SYS_ADMIN` to steal one). So
     every detach shape *loses* the grant. It never gains one.

     On macOS (CAD-315) the peer pid comes from `LOCAL_PEERPID`. If
     the platform cannot give that peer's session and its leader's
     start time reliably, `unlock` is not offered, and every operator
     act is signed per act.
   - **`login`:** mints ADR 0004's single-use link. The browser session
     then holds operator authority for ADR 0004's lifetimes.
   - **`delegate`:** see §5.5.
4. **Narrowing layer.** `operator_proof`'s checks still run on every
   operator request, whatever grant it carries, and a failure refuses.
   A grant never overrides "this caller descends from a registered pane".
   An `unlock` session that contains an agent pane refuses, and the
   refusal says so.
5. **`cadence operator lock`** revokes this session's unlock.
   `cadence operator grants` lists the live grants, and `--revoke-all`
   revokes every one of them.

### 5.3 The prompt is the defence against a confused signature

A hardware touch or a typed passphrase is only as good as the operator's
knowledge of *what* they are approving. The statement is signed, not a
hash of it. The CLI always prints it first. The daemon refuses an `act`
grant for any action other than the one in the statement. An agent that
sends `cadence approval record …` into the operator's terminal with tmux
`send-keys` produces a visible prompt that names the PR and the head.
The operator has to touch or type to complete it. That is the residual
R2 in §8, and it is stated, not hidden.

### 5.4 What each caller gets — the one table

| Caller | Public reads | Agent-allowed writes | Routine operator writes | Human-class acts |
|---|---|---|---|---|
| Attributed agent (pane ancestry or enrolled endpoint) | yes | yes, as that agent | refused | refused |
| Attributed agent **holding a delegation** (§5.5) | yes | yes | the delegated verbs, stamped `<alias> for operator` | refused |
| Operator: live `unlock` grant in this session, or a board session | yes | yes, as `operator` | yes | only with a per-act signature |
| Operator: per-act signature | yes | — | yes | yes, and the signature is stored |
| **Unattributable local caller with no grant** (any detach shape, the operator's shell before unlock) | yes | **refused** | **refused** | **refused** |
| Another uid, or the peer cannot be read | yes (the board's rule for reads, unchanged) | refused | refused | refused |

**The refusal text** names the rule and the remedy, for example: `slot
reconcile is an operator action; this connection holds no operator
grant. Run 'cadence operator unlock' in your terminal, then retry`.

**On attribution.** An unattributable caller is never stamped
`operator`, and it is not stamped with an agent's name either. It is not
stamped at all, because it is refused. The request field `by` is never
read as identity on any write verb (a pattern-matching gate already
refuses it on the approval verbs).

### 5.5 Delegation to a PM session

Today the operator's PM runs as a Claude Code session. Its writes are
unattributable, so they are stamped `operator`: for example, the
dispatch comment on CAD-280 at 2026-09-23T18:25:41Z reads `comment
operator`. Under P1 those writes are refused.

The fix is not to make the PM the operator. The fix is to make it
**attributable and delegated**:

- **Attributable:** the PM runs as a cadence-launched pane (`cadence join
  … --tui`) or as an enrolled managed endpoint, so its writes carry its
  alias (for example `op-pm`).
- **Delegated:** `cadence operator delegate op-pm --verbs
  agent-set,agent-remove,slot-reconcile --until 24h` produces a
  `delegate` statement. The operator signs it once. The daemon then
  honours those verbs from `op-pm`'s attributed connection, stamps them
  `op-pm for operator`, and emits `operator_delegation_used`.
- **Human-class acts are never delegable.** Approval records, plan
  decisions and rollout force-release always need the operator's own
  per-act signature. This keeps separation of duties: an author cannot
  approve its own work (the charter, and ADR 0001 §4 phase 3).

§9 Q4 asks the operator which verbs may be delegated at all.

## 6. Migration

### 6.1 Order (one implementation lane per step, each with its own risk class)

1. **The verifier and enrolment.** `operator enroll`, `challenge`,
   `prove`, `unlock`, `lock`, `grants`, plus signed records in the store.
   Nothing checks them yet. Test keys are added to the harness (§10), and
   `operator_rpc` becomes "sign with the test key".
2. **Human-class verbs, enforced.** `approval_record` and `revoke`, `plan
   approve` and `reject`, and `rollout release --force` require an `act`
   signature. `cadence audit` gains `verified` (the signature checks out)
   next to `operator-claimed` (a legacy row). CI verifies against the
   committed signers. `docs/AUDIT.md`'s "unverified until CAD-280"
   becomes "verified when signed".
3. **Routine operator verbs, in `observe` then `enforce`.** These are
   `slot_reconcile`, `agent set/remove/gc`, `agent_register` (the scope
   note on CAD-280 from PR #187), `model_defaults_set` (CAD-337), and
   every Class 2 and Class 3 verb in §1.2 that stamps `operator` today.
   The Class 4 tracker writes either move behind a daemon RPC, or keep
   their direct write but stamp `unproven` instead of `operator` (§9 Q6).
   - In `observe`, a call with no grant is allowed and logged as
     `operator_grant_would_refuse`, with the verb and the peer shape.
     This finds every script, the PM included, that still relies on the
     leftover rule.
   - The PM moves to a pane and a delegation (§5.5) *before* `enforce`.
4. **The board.** ADR 0004's session store and route table, with the
   link minted by a `login` grant rather than `operator_proof` plus the
   0600 file. ADR 0004's `observe` → `enforce` rollout applies unchanged.
5. **Flip P1 everywhere and remove the leftover rule.** The `Ok(None)`
   arm of `write_caller` refuses. Every `unwrap_or("operator")` on a
   write path is removed. `operator_proof` is renamed for what it now is
   (a narrowing check, `not_an_agent_shape`).

### 6.2 `slot_reconcile`, `approval_record` and board writes, concretely

| Surface | Today | After step 5 |
|---|---|---|
| `slot_reconcile` | `slot_identity` is `None`, then `proven_operator` | `operator_grant(conn, act)` with an `unlock` or `act` grant, or a delegation that includes `slot-reconcile`. The narrowing layer still runs. |
| `approval_record` / `revoke` | `approval_operator` (field refusal, `slot_identity`, `proven_operator`) | Field refusal kept. `act` signature required. The record stores `{statement, signature, key fingerprint}`. `audit` verifies it. |
| Board write routes | `write_caller`: `Ok(None)` gives `operator (ui)` | ADR 0004's `board_caller`. With no session and no pane tie, it refuses. A session exists only after a `login` grant. |

### 6.3 Existing installs, this host included

- The operator enrols a key before step 3 reaches `enforce`. `cadence
  doctor` and `ui status` report "no operator key enrolled", with the
  command to run.
- Legacy approval rows stay `operator-claimed` forever. They are not
  re-signed after the fact, because re-signing an old decision would
  launder it.
- The sandbox (CAD-310) has its own state dir, and so its own signers
  and grants. Test keys may be enrolled there. They are never enrolled
  in the production state.

### 6.4 Break-glass

| Situation | Path |
|---|---|
| Key lost or broken | Enrol **two** keys at setup, for example a hardware key and a passphrase key kept offline, and use the second one. With no second key, see the last row. |
| An unlock is suspected of being misused | `cadence operator grants --revoke-all`. Then look at `operator_grant_used` events. |
| `ssh-keygen` is missing or broken | Operator verbs refuse, and reads still work. It is installed with OpenSSH on every supported platform. |
| Daemon down | Nothing can be granted. `rollout release --force` keeps its daemon-stopped path (CAD-292) but now verifies an `act` signature itself, against the state-dir signers. |
| All keys lost | Re-bootstrap. Stop the daemon, then from an operator shell that passes today's `operator_proof`, run `cadence operator enroll --bootstrap`, which requires the daemon to be stopped and is logged loudly at the next start. This is the one place the old heuristic survives, and it is listed as residual R4. There is deliberately **no** "legacy operator" switch. |

## 7. The accepted residuals, re-evaluated (CAD-280 acceptance item 3)

This section is the proposal. The implementation lane closes or
re-states each residual in code and docs.

| Residual | Where it is documented today | Under this ADR |
|---|---|---|
| **CAD-276, the reconcile detach.** `env -u CADENCE_ALIAS setsid -f` passes `operator_proof`. | `peer.rs` `operator_proof` doc. `docs/AUDIT.md`. | **Closed.** A detached process holds no grant: it left the unlocked session, and it cannot sign. `operator_proof` no longer grants anything, so its residual is no longer an authority path. |
| **CAD-288, cadence's own tmux server (`new-window`).** | CAD-276 QA comment | **Closed** for the same reason. A new tmux window is a new session with no grant. The narrowing-layer fix (refuse the cadence tmux server hop) becomes optional hardening. |
| **CAD-263 / CAD-276, the borrowed pane pty (lateral authorship).** A same-uid process opening another pane's `/dev/pts/N` is attributed as that agent. | `peer.rs` module doc. `docs/BOARD.md`. Test `ui_write_caller_pty_tie_is_forgeable_residual_pinned`. | **Re-stated, and the kept tie is dropped for attribution.** It was kept only because dropping it sent a `setsid` child of a pane back to `operator (ui)`, an escalation (the PM's decision on CAD-276, 04:11Z). Under P1 that child is refused instead. Dropping the pty tie therefore escalates nothing, and it removes the lateral forgery. The pty tie stays a **narrowing** signal (it can refuse, never attribute), like `CADENCE_ALIAS` today. The pinned test flips to "refused". |
| **CAD-263, gateway and ssh-tunnel relays cannot be attributed.** | `docs/BOARD.md` | **Re-stated.** Unattributable callers are refused for writes. The operator's browser behind a tunnel works through a session. |
| **CAD-335, managed agents write to the board as the operator.** | CAD-335 | **Closed** by P1. The managed-endpoint attribution from CAD-335 PR #193 still attributes them as themselves. |
| **qa-pr187 F1, `agent set` from `setsid -f env -i`.** | CAD-280 comment | **Closed** by step 3. The verb needs a grant or a delegation. |
| **The approvals are operator claims.** | `docs/AUDIT.md` | **Closed for new records** (a verified signature). Legacy rows stay claims. |

## 8. Residuals this ADR accepts

| # | Residual | Class | Why it is accepted | Pinned by |
|---|---|---|---|---|
| R1 | Root (NOPASSWD sudo on this host) can patch the daemon and grant anything live | A3 | No in-host design survives root. Signed records stay verifiable off the host. §9 Q1. | Documented. Not testable in CI. |
| R2 | An agent can drive an **unlocked** operator terminal (tmux `send-keys`, the operator's tmux server) and use the routine grant until its TTL lapses | A2 | This is the unlock trade-off (sudo's timestamp has the same one). Human-class acts still show a prompt and need a touch or a passphrase. | A test: `send-keys` into an unlocked fixture session succeeds for a routine verb and produces a visible prompt for a human-class verb. |
| R3 | A trojaned `cadence` or `ssh-keygen` on `PATH` captures a passphrase, or triggers an unexpected touch | A2 | A binary the uid can replace cannot be trusted by that uid. A hardware key limits this to one confused touch, never to a key. | Documented. `cadence doctor` checks that the release symlink matches the release manifest. |
| R4 | Bootstrap and all-keys-lost fall back to `operator_proof` with the daemon stopped | A1 (narrow) | Bootstrap needs *some* first proof. It requires a stopped daemon, which is an overt act, and it is logged at the next start. | A test: bootstrap refuses while the daemon runs. |
| R5 | Anything the operator runs **inside** an unlocked terminal inherits the routine grant, for example an agent CLI started by hand in that shell | operator's choice | The same as sudo. The narrowing layer refuses registered panes and endpoints. An un-cadenced agent the operator started is the operator's delegate by their own act. | Documented in the `unlock` output. |
| R6 | A2 deletes or rolls back signed approval rows | A2 | This cannot be prevented on one uid. The audit already reports a missing approval as a finding, and CI verification catches a substituted row. | The existing audit tests, plus a new tampered-row test. |

## 9. Questions the operator must answer

1. **Sudo posture.** Agents run as a uid with NOPASSWD sudo, so every
   live guarantee here is a guardrail against A1, not a boundary against
   a determined agent. Do you accept that for now? Or do you want Option
   D, with agents under a uid without sudo, as a scheduled follow-up, and
   if so, when?
2. **Key kind.** A FIDO2 hardware key (human presence; recommended), a
   passphrase key, or both enrolled (one as recovery)? Do you have a
   FIDO2 token that works on the machine you operate from, and does that
   machine reach this host over ssh with `-sk` agent forwarding, or
   would you sign locally with a pasted challenge (§5.2 works either
   way)?
3. **Which acts need a per-act signature?** The proposal is approval
   record and revoke, plan approve and reject, and rollout
   force-release. Should merge (CAD-79 `cadence land`) and `agent set
   approval_policy=…` join them?
4. **Delegation.** May a PM session hold a signed, time-boxed delegation
   for routine operator verbs (§5.5)? Which verbs? What is the maximum
   lifetime?
5. **Unlock TTL.** What default and maximum? The proposal is 8 hours
   idle-free, and a 12-hour maximum.
6. **Unattributable callers and agent-allowed writes.** The proposal
   refuses them (for example `cadence issue comment` from a plain shell
   before unlock). Alternatively, stamp them `unproven-local`, with no
   authority beyond commenting. Which do you want?
7. **Relationship with ADR 0004.** Do you accept that ADR 0004's mint
   factor (`operator_proof` plus the 0600 file) is replaced by a signed
   `login` grant, with the rest of ADR 0004 unchanged? Or should the
   board keep the 0600 file as an extra factor?
8. **CI verification.** May `.cadence/operator_signers` (public keys) be
   committed, so that CI verifies human-class approvals on every merge?
   Changing it would then be a `human`-class change.
9. **Rollout.** One `observe` release for the routine verbs (§6.1 step 3),
   or enforce immediately once the PM is moved to a pane and a
   delegation?
10. **The lateral pty tie (§7).** Drop it for attribution now that
    unattributed callers are refused, or keep it and keep the lateral
    residual?

## 10. Test plan (acceptance item 2)

The tests extend `tests/integration.rs` and `tests/board.rs`, against a
temporary state dir and a random port. Keys are generated per test with
`ssh-keygen -t ed25519 -N ''` into the temp dir. CI cannot touch a
hardware key, so the `-sk` path is covered by a unit test of the
statement format plus a documented manual check.

### 10.1 The detach-shape matrix (T-A)

Iterate **every operator verb** × **every shape**. The verb list comes
from one table in code, so a new operator verb cannot be missed. Each
cell asserts **refused, with the named remedy, and no state change**.

The shapes, each run from a fixture pane:

1. a plain child of the pane;
2. `setsid -f`;
3. `setsid -f env -i PATH=… </dev/null >/dev/null 2>&1` (qa-pr187 F1);
4. a double fork with the intermediate exiting (an orphaned session);
5. an agent-started tmux server (`tmux -L x new -d …`);
6. cadence's own tmux server, `new-window` (CAD-288);
7. `systemd-run --user --pipe`, where CI has a user manager. Where it
   does not, the test asserts that the probe found none and prints that.
   It does not skip silently.
8. a managed endpoint's tool shell;
9. a board `curl` from each of the shapes above.

**The same matrix with the operator's grant present elsewhere.** Unlock
a fixture operator session, then run shapes 2–9 **from the unlocked
session**. Every one is refused, because detaching loses the grant. This
is the test that proves a grant cannot be carried out of its session.

### 10.2 The operator path (T-O)

- Every verb succeeds with an `act` signature by the enrolled test key.
- Routine verbs succeed from inside the unlocked session.
- The following are refused: a key that is not enrolled, a replayed
  nonce, an expired statement, another daemon's id, a statement for a
  different act, a signature under the wrong namespace, and an unlock
  presented from a different sid or with a mismatched leader starttime
  (a pid reused).
- `operator lock` and `--revoke-all` end the grants.

### 10.3 The records (T-R)

- An approval signed for head X does not verify for head Y.
- A row written straight into `cadence.sqlite3` with a made-up signature
  is reported `forged`. A legacy row is reported `operator-claimed`. A
  deleted row is reported missing (as today).
- CI's verifier, run against the committed signers, gives the same
  answer as `cadence audit`.

### 10.4 Delegation (T-D)

- A delegated verb from the delegate's pane succeeds, stamped `<alias>
  for operator`.
- The following are refused: a non-delegated verb, another agent using
  the delegation, a human-class verb, and any verb after the expiry.

### 10.5 Tests that flip on purpose

- `operator_rpc` stops using `setsid -f`. `operator_rpc_is_the_operator_even_from_an_agent_runner`
  becomes "is the operator because it signs".
- `ui_write_caller_ignores_an_uncorroborated_env_alias`: author
  `operator` → refused.
- `ui_write_caller_pty_tie_is_forgeable_residual_pinned` → refused (if
  Q10 drops the tie).
- The `rollout.rs` operator-probe tests gain the signature path.

### 10.6 Gates for the implementation lanes

- `cargo fmt --check`, clippy with `-D warnings`, `cargo test --lib
  --bins --test board`, and the named integration tests above. Each
  lane is risk class `human`, needs independent security review, and
  needs operator acceptance of this ADR first.

## 11. Consequences

- **Good:**
  - "Operator" becomes a thing someone *has*, not a thing left over.
  - The arms race against process shapes ends: new shapes need no new
    code.
  - The audit can finally say "verified".
  - The board and the CLI share one authority.
  - It is portable, and it needs no new crates.
- **Bad:**
  - The operator must own and handle a key, and unlock each terminal.
  - The PM needs a pane and a delegation.
  - Scripts that relied on the leftover rule break: the `observe` step
    finds them first.
- **Unchanged and stated:** on this host, root is one `sudo` away for
  every agent. Until Q1 is answered, this design ends *accidental and
  API-level* operator impersonation, and makes the rest deliberate and,
  for human-class decisions, verifiable. It does not make a determined
  agent incapable of it.
