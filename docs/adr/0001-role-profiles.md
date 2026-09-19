# 0001 — Role profiles: the team as committed data, with enforced duties

- Status: **proposed**
- Date: 2026-09-19
- Author: `arch-1` (architect)
- Deciders: operator (schema location, enforcement scope), PM (ticket order)
- Issues: CAD-76 (this), CAD-75 (parent epic), CAD-78, CAD-79, CAD-110
- Supersedes: nothing. First ADR.

## 1. Context

`docs/TEAM.md` describes a seven-role operating model. `docs/CHARTER.md`
puts "research and design agents turn goals into specs" at L4 and names
the operating model in the product as roadmap item 3. Today the model
exists in three places, none of which the software can read:

1. **Prose** — `docs/TEAM.md` and `docs/roles/*.md` (PR #65).
2. **An operator's shell history** — the `cadence join` invocations that
   created `rsch-1`, `arch-1`, `qa-1`, `ops-1` on 2026-09-19.
3. **Nothing durable per agent** — see the measurements below.

### 1.1 What is measurably true right now

Taken from the live host on 2026-09-19, not inferred:

| Claim | Evidence |
|---|---|
| `role` is a write-only field with two legal values | Validated once, at `store.rs:1028` — `if !matches!(new.role, "pm" \| "worker")`; `daemon.rs:1146` defaults it to `"worker"`; six `clap` flags default to `"worker"` (`main.rs:89,140,191,284,428,931`) and the flag is a bare `String` with no `value_parser`, so a bad role fails at the daemon, not at parse. |
| Nothing reads it back | `row_agent` reads the column (`store.rs:313`), `to_json` echoes it (`store.rs:340`), and no branch anywhere consumes it. Group and PM identity are derived entirely from `params.upstream`. The only consumer in the tree is cosmetic: `{a?.role ?? "agent"}` in `ui/src/components/Agents.tsx:106`. Audit N9 confirmed. |
| Every role agent is literally a "worker" | `cadence agent list`: `arch-1`, `qa-1`, `ops-1`, `rsch-1`, `devin-c`, `devin-d` all report `role: worker`. The reviewer and the DevOps agent are indistinguishable from a coder to the daemon. |
| Role identity is not persisted anywhere else | `params` for `qa-1` and `arch-1` are exactly `{effort, model, permission_mode, upstream}`. Nothing says "reviewer". |
| Briefings did not reach their agents | `cadence agent show qa-1` advertises `briefing: …/briefings/fable-cc/BRIEFING-qa-1.md`. That file **does not exist**. Same for `ops-1`, `arch-1`, `rsch-1`. The only file in the briefings tree is `BRIEFING-ci-claude.md`. |
| The agent record has no instructions to show | `cadence agent show qa-1` has no `instructions` key. `Agent::to_json` (`store.rs:337-362`) omits the column, and `rpc_show` (`daemon.rs:980-1006`) does not add it — although `store.rs:1038` accepts and length-caps `NewAgent.instructions` at 32 000 chars. |
| Consequence, observed | Each of the four role agents had to be told in its kickoff message to go and read a file path. The operating model was delivered by hand, per agent, per session. That is CAD-110. |

Two facts settle questions that would otherwise need debate:

- **The tracker is a git repository.** `~/pm` has remote
  `github.com/favcrm/pm` and commits on every mutation
  (`CAD-76: start cadence/cad-76-adr-role-profiles`). A file at
  `~/pm/<project>/team.yaml` is therefore *committed, versioned and
  reproduced by a clone* — which is what "a fresh host reproduces the
  team" requires, and what charter principle 5 asks for.
- **No new dependency is needed.** `Cargo.toml` already carries
  `serde_yaml = "0.9"` *and* `toml = "0.8"`. The tracker already speaks
  YAML (`~/pm/pm.yaml`, `~/pm/<project>/project.yaml`, the latter with a
  `schema: 1` key). So `team.yaml` beside `project.yaml` is the existing
  idiom and costs nothing; it does **not** trip risk-class trigger 4
  (supply chain).

### 1.2 CAD-110 is broader than the ticket says

CAD-110 reads as "`--instructions-file` is silently ignored **with
`--no-bootstrap`**". The code says something stronger, and the design
depends on the difference:

- `--instructions-file` is read **unconditionally**, before and
  independent of any bootstrap decision (`main.rs:4295`), and stored on
  the agent record.
- The stored value has **exactly one reader in the whole codebase**:
  `adapter/codex.rs:337-338`, which maps it to codex's
  `developerInstructions`.
- Therefore for `provider = claude | devin | cursor`, the text is stored
  and **never read at all** — with or without `--no-bootstrap`. Claude's
  argv builder (`adapter/claude.rs:106-158`) reads only `params.*`; no
  pty profile touches it; `briefing_body` (`main.rs:4713-4852`) never
  references it either. There is no code path where the
  `--instructions-file` content and the briefing meet.
- `--no-bootstrap` separately suppresses the *other* channel: it sets
  `BriefMode::Off` (`main.rs:4568-4572`), so the single gate at
  `main.rs:4451-4463` never calls `brief_agent`, and the briefing file,
  the AGENTS.md block and the durable `bootstrap-<alias>` message are all
  skipped.

So two independent defects produced one symptom. Every role agent on this
host runs managed Claude, which means **the role briefing had no delivery
channel at all**, and fixing the flag combination alone would not have
delivered one. This needs to go back to the PM as a correction to
CAD-110's scope (§8.5).

### 1.3 The one fact that reframes the whole problem

`docs/TEAM.md` says the reviewer never merges and the author never
reviews. The daemon cannot currently observe either, because **merging
does not go through cadence**. `ops-1` merges with `gh pr merge`. A
rule like "a reviewer cannot merge" is unenforceable against a process
that can invoke an unrelated binary.

Therefore: **enforcement is only as real as the surface it owns.**
Anything cadence does not mediate can be *detected after the fact* (who
merged, compared with who reviewed) but not *refused*. This makes
CAD-79's `cadence land` a **prerequisite** for enforcement, not a
sibling of it — a dependency the epic currently does not record.

### 1.4 Threat model, stated honestly

All agents on this host run as the same unix user, several in bypass
permission mode, and identity arrives as `CADENCE_ALIAS` in the
environment — which any process can set. So:

> Role enforcement in cadence is a guardrail against **agent error and
> instruction drift**, not against a **hostile local process**. It makes
> the wrong action fail loudly instead of quietly succeeding.

This is worth writing down because the next person to read
`team.yaml` will otherwise assume it is a security boundary and build on
sand. Where a stronger binding is available we use it: the per-turn
token is minted by the daemon and already validated, so it, not the
alias, is the actor proof for anything consequential.

## 2. What "done" looks like

Grilled down to five testable statements. An option is judged against
these, not against how complete it feels.

- **D1 Reproduce.** On a host with a clone of the product repo and the
  tracker and no shell history, one command brings up the team as
  `docs/TEAM.md` describes it, and a second command shows it matches.
- **D2 Identity.** `cadence agent show qa-1` states the agent's role and
  the briefing it is operating under, and that briefing exists.
- **D3 Duties.** An author cannot record a pass verdict on its own
  issue; the recorder of a verdict cannot land it; both refusals name
  the rule and the two conflicting actors.
- **D4 Approval.** A class-`human` merge cannot proceed without a
  durable operator approval naming the exact full head SHA, and is
  refused if the head moved.
- **D5 Drift is visible.** When the running team differs from the
  declared team, something says so without being asked.

Non-goals for this ADR: multi-level group trees (CAD-77), per-role
budgets (CAD-80), cross-project teams (CAD-82).

## 3. Options

### Option A — Do nothing

Keep the model in prose; keep creating agents by hand.

- Cost: zero.
- D1 ✗ D2 ✗ D3 ✗ D4 ✗ D5 ✗.
- What it actually costs: the four role agents joined on 2026-09-19 each
  needed a bespoke kickoff to learn their own job, and the `role` field
  keeps claiming everyone is a worker. Every new project repeats the
  ritual. Drift between `docs/TEAM.md` and reality is undetectable by
  construction — the docs are already wrong about `role` and nothing
  noticed.
- Rejected, but it is the correct baseline for judging the rest: A is
  survivable, which is why the phased plan below is allowed to take its
  time.

### Option B — The smallest thing that could work

Two changes, no schema, no daemon change:

1. `scripts/team-up.sh` checked into the repo: the literal `cadence join`
   lines for the six roles, with `--instructions-file docs/roles/<role>.md`.
2. Make the CAD-110 flag combination **fail closed**: `join
   --instructions-file … --no-bootstrap` refuses with a message naming
   both flags, and the advertised `briefing` path is verified to exist
   (no dangling pointers).

- D1 ✓ (a script is a reproduction) D2 partial D3 ✗ D4 ✗ D5 ✗.
- Honest assessment: this delivers the *reproduce* half at roughly one
  percent of the cost of Option D, and (2) is a real bug fix regardless
  of which option wins. Its weakness is not that a script is ugly — it
  is that a script cannot be *queried*: nothing can ask "what model
  should qa-1 be on?" and compare. No drift detection, no enforcement,
  and per-project teams mean per-project scripts.
- **Not rejected.** It is adopted as phase 1 (§4).

### Option C — Declaration only: `team.yaml`, no enforcement

`~/pm/<project>/team.yaml` declares roles; `cadence join --role qa`
expands a profile into launch params; `cadence team up|down|show`
instantiates or parks the tree; `team show` reports drift. The `role`
field becomes a real value from the declared set. No authority checks.

- D1 ✓ D2 ✓ D3 ✗ D4 ✗ D5 ✓.
- Strength: it is the whole of CAD-76 as written, it is cheap (both
  parsers are already present), and it is almost entirely additive —
  plausibly risk class `auto` apart from the `role` validation change.
- Weakness: separation of duties stays a convention that only holds
  while every agent reads its briefing correctly. The charter calls
  separation of duties a *principle* (§Principles 2); a principle that
  the system cannot check is a hope.

### Option D — Declaration plus daemon-enforced duties

Option C, plus: roles carry **capabilities**; the daemon refuses a
capability the acting role does not hold; duty separation is a predicate
over actors per artifact; class-`human` merges require a durable
operator approval record pinned to a full SHA. Requires `cadence land`
(CAD-79) to exist so that merging is a surface cadence owns.

- D1 ✓ D2 ✓ D3 ✓ D4 ✓ D5 ✓.
- Cost: touches the trust boundary, the merge path and the store →
  risk class `human` by triggers 1, 2 and 6. Cannot land as one PR: it
  is several, each needing an operator.
- Weakness: enforcement scope is bounded by §1.2 — it binds cadence
  verbs, not `gh`. Sold as more than that, it would be a lie.

### Option E — Rejected: roles as prompt text only

Express each role purely as a system prompt / briefing ("you must not
merge") and add nothing to the daemon.

Rejected explicitly because this is what we have been doing, and
2026-09-19 measured its failure rate: the briefings were not even
delivered (§1.1), and on CAD-62 a PM's wrong diagnosis was acted on by a
worker before it could be retracted. Instructions degrade; a refusal
does not. Charter principle 1: a worker's "I won't" is a claim.

### Option F — Rejected: literal command allowlists per role in config

CAD-79 asks for "an explicit allowlist of state-changing commands".
Read literally that puts command strings and globs in `team.yaml`.

Rejected as the *config* representation: literal allowlists rot on the
first flag rename, must be edited in every project's `team.yaml`, and
invite one permissive glob that silently grants everything. The
allowlist still exists — as a capability→command table **in code**,
where it is reviewed and versioned with the commands it names. Config
names capabilities; code maps them. (A config file listing every command
is also the deep-branch-tree smell: the shape is wrong, not the size.)

### Option G — Rejected: operator approval by matching chat text

The routing table in `docs/TEAM.md` has the operator sending the phrase
`OPERATOR APPROVED #<pr> at <full sha>` as a message, which `ops-1`
reads and believes.

Rejected as the enforcement mechanism: pattern-matching prose in a queue
that agents also write to means an agent can satisfy the gate by
quoting the phrase — in a summary, a retro, or a quoted-back kickoff.
Approvals become durable records with their own verb (§5.4). The phrase
survives as the *human-facing* way to create one, not as the check.

## 4. Decision

**Adopt Option D, reached in three phases, each a separate PR with its
own risk class. Phase 1 is Option B verbatim.**

| Phase | Content | Satisfies | Risk class |
|---|---|---|---|
| 1 | `scripts/team-up.sh`; CAD-110 fail-closed (refuse a no-op `--instructions-file` — both the flag combination and the providers with no reader — and verify the advertised briefing path exists) | D1, part of D2 | `auto` for the script; `human` for the join/briefing change (trigger 1) |
| 2 | `team.yaml` schema + `cadence join --role` + `team up\|down\|show` with drift report; `role` becomes a real value; briefing text attached to the agent record and replayed | D1, D2, D5 | mostly `auto`; the `role` validation and instructions replay are `human` (triggers 1, 2) |
| 3 | Capabilities, duty predicate, approval records — **gated on `cadence land` (CAD-79)** | D3, D4 | `human` (triggers 1, 2, 6) |

Why phased rather than one design landed at once:

- Phase 1 is the tracer bullet. Writing the six literal `join` lines is
  the cheapest possible test of whether the field list in §5.1 is the
  right field list. If a role cannot be expressed as a `join` invocation
  today, no schema will fix that, and we would rather learn it from a
  shell script than from a migration.
- Phase 2 is worthless before phase 1 exists and worth a lot after: it
  is the same information, queryable.
- Phase 3 is the only part that touches the trust boundary, and putting
  it behind its own phase keeps phases 1–2 out of the operator's queue.

## 5. Design

### 5.1 `team.yaml` — location, shape

**Location: `~/pm/<project>/team.yaml`**, beside `project.yaml`, as
CAD-76 states. Reasons, now evidenced rather than assumed: the tracker
is a git repo with a remote (§1.1), so the file is committed; a project
may own several repos (`project.yaml` has a `repos:` list) while it has
exactly one team, so the team is a property of the project, not of a
repo; and the tracker already reads YAML with a `schema:` key.

```yaml
schema: 1

defaults:                       # every role inherits, then overrides
  provider: claude
  kind: managed
  model: opus
  effort: high
  permission_mode: bypassPermissions
  worktree: none
  max_concurrent: 1

roles:
  pm:
    kind: inbox
    alias: fable-cc            # fixed alias, not a pattern
    capabilities: [dispatch, plan, issue-write, docs-write]

  research:
    alias: rsch-{n}
    briefing: docs/roles/research.md
    skills: [research, firecrawl, cf-crawl]
    capabilities: [issue-comment, artifact-write]

  architect:
    alias: arch-{n}
    briefing: docs/roles/architect.md
    skills: [grilling, codebase-design, domain-modeling]
    worktree: own
    capabilities: [issue-new, docs-write, pr-open]
    limits: { docs_only: true }

  dev:
    provider: devin
    kind: pty
    permission_mode: dangerous
    alias: dev-{n}
    briefing: docs/roles/dev.md
    worktree: own
    max_concurrent: 5
    capabilities: [pr-open, issue-comment]

  qa:
    alias: qa-{project}
    briefing: docs/roles/qa.md
    skills: [code-review, security-assessment]
    worktree: detached          # guardrail 1, as data
    capabilities: [review, verdict, issue-new, memory-accept]
    decorrelate: model          # prefer a different model than `dev`

  devops:
    effort: medium
    alias: ops-{n}
    briefing: docs/roles/ops.md
    capabilities: [land, restart, deploy, worktree-gc, host-care]
    limits: { merge_class: auto }   # class `human` needs an approval record
```

Field notes, each with a reason. Several of these are forced by what the
endpoint registry already accepts — `registry::validate_launch_params`
rejects any `params` key not listed in that endpoint's
`launch_params` (`adapter/registry.rs:122-388`), so a profile field that
does not map to an accepted key is a refusal, not a silent drop:

- **`kind` is not a flag.** `join` selects the pty endpoint with `--tui`
  and otherwise takes `registry::default_kind(provider)`
  (`main.rs:4224-4237`). `kind: pty` in a profile therefore expands to
  `--tui`, and `kind: managed` to its absence. Likewise `provider` is a
  **positional** argument to `join`, not `--provider`.
- **`effort` is a Claude-only parameter today.** It appears in
  `launch_params` for `claude managed` and `claude pty` only; `codex` has
  neither `effort` nor `model`, and `devin` has neither. So
  `defaults.effort: high` inherited by a `devin` role would be *rejected
  at register*. A profile must therefore drop provider-inapplicable keys
  at expansion and say so, or refuse — §8.1 decides which after `rsch-1`
  reports. Legal Claude values are `low|medium|high|xhigh|max`
  (`registry.rs:633`).
- **`sandbox` must be a profile field.** `join` hardcodes
  `sandbox: "read-only"` (`main.rs:4406`), and `sandbox` is one of the
  very few fields that *is* read back — `adapter/codex.rs:334` passes it
  into codex `thread/start`. So a joined codex worker can never be
  `workspace-write` today; a `dev` role on codex is currently
  unexpressible. This is a latent bug the schema work should fix.
- **`permission_mode` vocabulary is per provider** and mostly validated:
  devin `auto|accept-edits|smart|dangerous` (`registry.rs:30`), cursor
  `auto-review|force` (`registry.rs:36`). Claude's modes are *not*
  validated by cadence (`registry.rs:721-723` — "provider-validated").
  `team up` should validate all three against the provider, since a typo
  in a committed file that only fails at spawn time is the silent-failure
  shape the charter argues against.
- **`--bypass` conflicts with `--permission-mode`** (`main.rs:467`), so a
  profile expresses one or the other, never both.

- **`briefing` is a path in the product repo**, resolved against the
  project's repo (`repo:` qualifier when the project has more than one).
  The content lives where PR #65 already put it — `docs/roles/*.md` —
  so it is reviewed like code. The tracker holds the *pointer*; the repo
  holds the *text*.
- **`skills` verifies, it does not install.** Skills are host-level
  (`~/.claude/skills`), and `src/skill.rs` installs exactly one skill —
  `cadence` — vendored into the binary. So a named skill that is absent
  is a **refusal at `team up`** with the missing names listed, not a
  silent degradation. (A role that silently lost `code-review` would
  review worse and nobody would know: fail closed, charter principle 3.)
- **`effort` stays a portable three-to-five-value field.** `managed`
  Claude maps it to the CLI's `--effort` (`low|medium|high|xhigh|max`,
  already validated at register per `docs/PROTOCOL.md`). Mappings for
  Codex (`model_reasoning_effort`) and Devin (`mode`) are an open
  question with `rsch-1` (§8); until answered, an `effort` on a provider
  with no mapping is a refusal at parse, not a silent drop.
- **`worktree: detached`** encodes guardrail 1 (review in a detached
  checkout, never the author's worktree) as data instead of as a habit.
- **`decorrelate: model`** is CAD-78's "prefer a different provider or
  model for qa than for code". It is a **warning at `team up`**, never a
  refusal — a single-provider host must still be able to run a team.
- **`alias`** is a pattern (`dev-{n}`, `qa-{project}`) so `join --role`
  can allocate; the PM's alias is fixed because it is an inbox.

### 5.2 Source of truth, and what happens on disagreement

The sharpest question in this design, because getting it wrong produces
a reconciler that fights live work.

> **`team.yaml` is the template at join time. The registry is the truth
> for a running agent. `cadence team show` reports drift and refuses to
> reconcile it silently.**

The reason is mechanical, not philosophical: most launch params are
launch-time only, and the code is strict about it. `registry::
validate_live_param` (`adapter/registry.rs:593-629`) admits exactly
`auto_ready` (pty) and `stall_secs`; `NEXT_LAUNCH_PARAMS`
(`adapter/registry.rs:651`) admits exactly `model` and `effort`, and only
for the *next* launch. `docs/PROTOCOL.md` says the rest — "relaunch or
rejoin to change it".
So "the yaml always wins" necessarily means *killing and relaunching
agents to converge* — a destructive loop over agents that may be
mid-turn. Instead `team show` prints declared-vs-running per field, and
`team up --converge` (explicit, never implicit) relaunches only agents
that are idle, refusing on any that are busy and saying which.

This satisfies D5 without inventing a controller that can eat work.

### 5.3 Capabilities, not command lists

Roles hold capability names from a closed set defined in code:
`dispatch`, `plan`, `issue-new`, `issue-comment`, `issue-write`,
`artifact-write`, `docs-write`, `pr-open`, `review`, `verdict`, `land`,
`restart`, `deploy`, `worktree-gc`, `host-care`, `memory-accept`.

Each cadence verb declares the capability it requires; the daemon checks
the acting agent's role against it and refuses with the rule, the role
and the missing capability. An unknown capability name in `team.yaml` is
a parse error — a typo must not read as "no restriction" (fail closed).

**Where the role and its capabilities are stored is not free.** They
cannot go in `params`: `store.rs:1046-1054` caps the whole JSON object at
4 000 characters, and `validate_launch_params` rejects any key an endpoint
does not declare, so `capabilities` would be refused at register. So
phase 2 needs either a widened `agents.role` (a real value from the
declared set, replacing the `pm|worker` check at `store.rs:1028`) plus a
resolved-capabilities column, or a small `agent_roles` table. Either is a
store-version change, which is risk-class trigger 2 — and it is the
reason phase 2 cannot be purely additive. The upside is already paid for:
`Store::recover` (`store.rs:664-773`) preserves `role`, `instructions`
and `params` across a restart, clearing only `pid`, `endpoint` and
`generation`, so role identity survives a daemon restart for free.

The actor is resolved from the **turn token** where one exists, falling
back to `CADENCE_ALIAS` only for verbs that are not consequential, with
the limits of that binding recorded in §1.4.

### 5.4 Separation of duties as a predicate over artifacts

The rule is not "role names differ" — that would let two `dev` agents
review each other's work, which is fine, while blocking a legitimate
second architect. The rule is over **actors on one artifact**:

| Rule | Predicate | Data it needs | Exists today? |
|---|---|---|---|
| An author may not pass its own work | `verdict.actor != issue.owner` and `verdict.actor` not the pusher of the PR head | issue owner (recorded by `issue start --owner`), PR head author | owner yes; head author via `gh` |
| A reviewer may not land | `land.actor != verdict.actor` | verdict actor, recorded with the verdict | needs a field |
| Only a `review`-capable role may record a verdict | role capability check (§5.3) | `team.yaml` | phase 2 |
| A class-`human` merge needs approval | an approval record for `<pr>` whose `sha` equals the current head | approval records (new) | no |
| The proposer of a memory lesson may not accept it | `memory.accept.actor != memory.propose.actor` | memory records | charter principle 2, unchecked today |

Approval records replace phrase-matching (Option G): `cadence approve
<pr> --sha <full-sha>` writes a durable record, refused if the caller
resolves to a registered agent rather than the operator, and consumed by
`land` only while the head still equals that SHA. This makes guardrails
3 and 4 mechanical instead of procedural.

Everything cadence does not mediate — a direct `gh pr merge` — is
**detected, not prevented**: `land` and the post-merge check record the
merging actor, and a mismatch against the verdict actor raises an
incident. Charter principle 6 ("detect, then automate") is the right
order here, and pretending otherwise would be the dishonest option.

### 5.5 Briefings: how the operating model reaches an agent (CAD-110)

Three things are conflated today and need separating:

1. `BRIEFING-<alias>.md` — cadence's generated *identity* file (alias,
   session id, upstream, roster). Useful, ephemeral, in the state dir.
2. `--instructions-file` → `NewAgent.instructions` — the *role briefing*.
   The column and its validation already exist in `store.rs`; nothing
   surfaces or replays it.
3. `docs/roles/<role>.md` — the *reviewed source text*, in git (PR #65).

And the delivery channel is **provider-shaped**, which §1.2 shows is the
part nobody had noticed. A role briefing can only reach an agent through
a channel its adapter actually reads:

| Provider / kind | Channel that exists | Replays on resume? | Work needed |
|---|---|---|---|
| `codex managed` | `developerInstructions` (`adapter/codex.rs:337`) | yes — set on every `thread/start`/`thread/resume` | none; this is the one that works |
| `claude managed` | none — `build_command` (`adapter/claude.rs:106-158`) reads only `params.*` | — | add a system-prompt launch param to the spec's `launch_params` and to argv, so it replays like `--model` does |
| `claude pty`, `devin pty`, `cursor pty` | none; a pane has no system prompt | — | the briefing file plus a bootstrap message is the only channel, so it must be **re-sent on resume**, not only on first open |

This is why "attach the instructions to the agent" (CAD-110's first
suggested fix) is not one change but three, and why the managed-Claude
case — the one every role agent on this host uses — is the one with no
channel at all.

Decision:

- `team.yaml` names the source path per role; `join --role` reads it and
  stores the **resolved text as a snapshot** on the agent record, with
  the source path and a content hash.
- The daemon replays that snapshot through the channel above on every new
  session and on resume — so a resumed `qa-1` is still a reviewer, which
  is precisely what failed today. For managed Claude that means a new
  launch param; for pty it means the bootstrap message becomes part of
  resume rather than of first open only.
- `cadence agent show` gains `instructions` (source path, hash, byte
  count) and the advertised `briefing` path is **verified to exist**.
  An advertised path that is not there is the dangling pointer measured
  in §1.1 and must read as an error, not as a field.
- `cadence team show` compares the stored hash against the file in git
  and reports "briefing drifted from `docs/roles/qa.md`".

Why a **snapshot** rather than re-reading the file each turn: a live
re-read means an agent's standing instructions change mid-flight when
someone edits `main`. CAD-75's own design note is that instruction
degradation across hops is the failure mode; an unreviewed, unversioned,
mid-turn instruction swap is the worst possible hop. Drift is reported,
and `team up --converge` is how you adopt it deliberately.

Briefings over 32 000 characters are refused at join with the size and
the limit, not truncated.

## 6. Consequences

**Good.** A fresh host reproduces the team from two clones. `role` stops
lying. Separation of duties moves from prose to predicate for the
surfaces cadence owns. Guardrails 1, 3 and 4 become data or checks.
Adding a role is a reviewed diff in a git-tracked file. Phase 1 is
shippable this week and is useful even if phases 2–3 never land.

**Bad, and accepted.** `team.yaml` is a second place where team facts
live, and `docs/TEAM.md` can drift from it — mitigated only by
`team show` and by treating the yaml as normative where they disagree.
Enforcement is bounded by §1.3 and §1.4: it stops mistakes, not a
determined local process. Phase 3 needs several operator approvals by
construction, so it is slow on purpose. And `serde_yaml 0.9` is
deprecated upstream — pre-existing, out of scope here, but this decision
puts more weight on it, so it is worth a ticket of its own.

**Ugly.** Until `cadence land` exists, "a reviewer cannot merge" is a
detection, not a refusal, and the ADR must not be quoted as saying
otherwise.

## 7. Acceptance checks

Commands `qa-1` runs. Each maps to a statement in §2.

**Phase 1**

```bash
# D1: a script, not a memory, brings the team up
test -x scripts/team-up.sh
grep -c 'cadence join' scripts/team-up.sh          # == number of roles

# CAD-110: the flag combination fails closed, naming both flags.
# `provider` is positional on join, and there is no --provider flag.
cadence join fable-cc claude --instructions-file docs/roles/qa.md --no-bootstrap
test $? -ne 0                                      # and stderr names both flags

# ...and it also fails closed for a provider that cannot read the text at
# all, which is the wider defect in §1.2 (one reader: codex).
cadence join fable-cc claude --instructions-file docs/roles/qa.md
test $? -ne 0                                      # until the channel exists

# no dangling briefing pointer: this currently fails for all four role
# agents and is the regression test for §1.1
for a in $(cadence agent list | jq -r '.agents[].alias'); do
  p=$(cadence agent show "$a" | jq -r '.agent.briefing // empty')
  [ -z "$p" ] || test -f "$p" || { echo "dangling: $a -> $p"; exit 1; }
done
```

**Phase 2**

```bash
# D2: role and briefing are real and visible
cadence agent show qa-1 | jq -e '.agent.role == "qa"'
cadence agent show qa-1 | jq -e '.agent.instructions.source == "docs/roles/qa.md"'
test -f "$(cadence agent show qa-1 | jq -r .agent.briefing)"

# briefing survives a new session (the actual CAD-110 regression)
cadence agent stop qa-1 && cadence agent resume qa-1
cadence agent show qa-1 | jq -e '.agent.instructions.hash != null'

# fail closed on a missing skill and on an unknown capability
cadence team up --dry-run          # refuses, listing missing skills by name
cadence team show                  # declared vs running per field, exit 1 on drift

# a typo must not mean "unrestricted"
printf 'capabilities: [revieww]\n' >> /tmp/bad-team.yaml
cadence team show --file /tmp/bad-team.yaml; test $? -ne 0
```

**Phase 3**

```bash
# D3: author cannot pass its own work; reviewer cannot land
cadence review verdict CAD-XX --pass          # as the issue owner -> refused,
                                              # message names both actors
cadence land <pr> --as qa-1                   # -> refused: missing capability `land`

# D4: approval is pinned to the exact head
cadence land <pr>                             # class human, no approval -> refused
cadence approve <pr> --sha <stale-sha>
cadence land <pr>                             # -> refused: head moved, prints both SHAs

# an agent cannot mint an operator approval
CADENCE_ALIAS=ops-1 cadence approve <pr> --sha <head>; test $? -ne 0
```

## 8. Open questions

1. **Effort mapping per provider.** Cadence plumbs `effort` for Claude
   only; codex and devin endpoints do not accept the key at all
   (`adapter/registry.rs:122-388`), and codex accepts no `model` either.
   So the question is not just "what is the knob called" but "should
   cadence plumb it": exact knob names and legal values for Codex
   (`model_reasoning_effort`), Cursor and Devin (`mode`), and whether each
   is launch-time only. Sent to `rsch-1` on 2026-09-19 against CAD-76.
   Until answered, a profile carrying `effort` for a provider with no
   mapping is refused at parse rather than dropped. Blocks freezing the
   phase-2 schema, not phase 1.
2. **Prior art for declarative duty separation** — whether GitLab's
   `prevent_author_approval`, GitHub required reviewers, or Kubernetes
   RBAC bind the rule to the actor identity or to a held capability, and
   which fails safer under §1.4. Same question to `rsch-1`. May change
   §5.3's choice of binding.
3. **Operator decision:** does `team.yaml` live in the tracker
   (recommended, §5.1) or in the product repo? A team spanning repos and
   a `project.yaml` sibling argue for the tracker; "committed with the
   code it governs" argues for the repo.
4. **`cadence land` scope** — CAD-79 currently reads as an agent role.
   Phase 3 needs it to also be the verb that owns the `gh` call. The PM
   should decide whether that is CAD-79 or a new ticket.
5. **CAD-110's scope is understated (PM action).** The ticket asks to
   either attach instructions or refuse the flag combination. §1.2 shows
   those are two separate defects and that refusing the combination fixes
   neither for managed Claude, which is what every role agent here runs.
   Recommend splitting: the refusal plus the dangling-briefing check stay
   on CAD-110 (phase 1, small, shippable now), and the per-provider
   instructions channel becomes its own ticket (proposed below) because it
   touches three adapters and the claude endpoint spec.
6. **`serde_yaml 0.9` is deprecated upstream.** Pre-existing and out of
   scope here, but this decision puts more weight on it. Worth its own
   ticket so the choice is deliberate rather than inherited.

## 9. How we would know this was wrong

- `team.yaml` files across projects turn out near-identical → the
  content belonged in code defaults, and the schema is ceremony.
- Drift reports from `team show` are routinely acknowledged and ignored
  → D5 is noise, and drift should either refuse or not be reported.
- A duty refusal blocks a legitimate merge more than once → the actor
  model in §5.3 is too coarse; move the binding to per-capability tokens.
- Phase 1's script never grows into phase 2 → the schema was never
  needed and Option B was the whole answer. This would be a *good*
  outcome cheaply learned, which is the point of phasing.
- Roles proliferate past about eight → the tree, not the profile, is the
  missing structure, and CAD-77 should have come first.
