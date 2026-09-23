# 0001 — Role profiles: the team as committed data, with enforced duties

- Status: **proposed**
- Date: 2026-09-19
- Author: `arch-1` (architect)
- Deciders: operator (schema location, enforcement scope), PM (ticket order)
- Issues: CAD-76 (this), CAD-75 (parent epic), CAD-78, CAD-79, CAD-110
- Supersedes: nothing. First ADR.
- **Code citations are pinned to `33a6a82` (main, 2026-09-20).** Line
  numbers move; the claims are what matter. Re-locate by the quoted
  symbol or comment rather than trusting a line number after main moves.

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
| `role` is a write-only field with two legal values | Validated once, in `register_agent` (`store.rs:1036`) — `if !matches!(new.role, "pm" \| "worker")`; `daemon.rs:1146` defaults it to `"worker"`; six `clap` flags default to `"worker"` (`main.rs:93,144,200,293,436,954`) and the flag is a bare `String` with no `value_parser`, so a bad role fails at the daemon, not at parse. |
| Nothing reads it back | `row_agent` reads the column (`store.rs:312`), `Agent::to_json` echoes it (`store.rs:343`), and no branch anywhere consumes it. Group and PM identity are derived entirely from `params.upstream`. The only consumer in the tree is cosmetic: `{a?.role ?? "agent"}` in `ui/src/components/Agents.tsx:106`. Audit N9 confirmed. |
| Every role agent is literally a "worker" | `cadence agent list`: `arch-1`, `qa-1`, `ops-1`, `rsch-1`, `devin-c`, `devin-d` all report `role: worker`. The reviewer and the DevOps agent are indistinguishable from a coder to the daemon. |
| Role identity is not persisted anywhere else | `params` for `qa-1` and `arch-1` are exactly `{effort, model, permission_mode, upstream}`. Nothing says "reviewer". |
| Briefings did not reach their agents | `cadence agent show qa-1` advertises `briefing: …/briefings/fable-cc/BRIEFING-qa-1.md`. That file **does not exist**. Same for `ops-1`, `arch-1`, `rsch-1`. The only file in the briefings tree is `BRIEFING-ci-claude.md`. |
| The agent record has no instructions to show | `cadence agent show qa-1` has no `instructions` key. `Agent::to_json` (`store.rs:343`) omits the column, and `rpc_show` (`daemon.rs:980-1006`) does not add it — although `store.rs:1048` accepts and length-caps `NewAgent.instructions` at 32 000 chars. |
| It is not the only dead role field | the `tasks` DDL's `role TEXT NOT NULL DEFAULT 'implementer'` (`store.rs:575`) is commented "reviewer/merger are M3b" and is display-only too. Two write-only role fields already; CAD-78 should make one of them real, not add a third. |
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
  independent of any bootstrap decision (`main.rs:4364`), and stored on
  the agent record.
- The stored value has **exactly one reader in the whole codebase**:
  `adapter/codex.rs:358-359`, which maps it to codex's
  `developerInstructions`.
- Therefore for `provider = claude | devin | cursor`, the text is stored
  and **never read at all** — with or without `--no-bootstrap`. Claude's
  argv builder (`adapter/claude.rs:106-158`) reads only `params.*`; no
  pty profile touches it; `briefing_body` (`main.rs:4796-4937`) never
  references it either. There is no code path where the
  `--instructions-file` content and the briefing meet.
- `--no-bootstrap` separately suppresses the *other* channel: it sets
  `BriefMode::Off` (`main.rs:4649-4655`), so the single gate at
  `main.rs:4538-4548` never calls `brief_agent`, and the briefing file,
  the AGENTS.md block and the durable `bootstrap-<alias>` message are all
  skipped.

So two independent defects produced one symptom. Every role agent on this
host runs managed Claude, which means **the role briefing had no delivery
channel at all**, and fixing the flag combination alone would not have
delivered one. This needs to go back to the PM as a correction to
CAD-110's scope (§8.5).

### 1.3 The one fact that reframes the whole problem

`docs/TEAM.md` says the reviewer never merges. The daemon cannot observe
that, because **merging does not go through cadence** — by design, so far.
There is no `cadence land` and no `cadence merge` anywhere in the tree.
`main.rs:839` states the policy: "Cadence never runs git merges itself."
`job accept --merged-sha` records a merge *claim* after the fact, and
`overview.rs:771` prints
`gh pr merge <n> --squash --admin --match-head-commit <head>` for a human
to copy. A rule like "a reviewer cannot merge" is unenforceable against a
process that can invoke an unrelated binary.

Therefore: **enforcement is only as real as the surface it owns.**
Anything cadence does not mediate can be *detected after the fact* (who
merged, compared with who reviewed) but not *refused*. This makes
CAD-79's `cadence land` a **prerequisite** for enforcement, not a
sibling of it — a dependency the epic currently does not record.

### 1.4 Threat model, stated honestly

All agents on this host run as the same unix user, several in bypass
permission mode, and identity arrives as `CADENCE_ALIAS` in the
environment — which any process can set. The code already says this about
itself, and the ADR should not claim more. `daemon.rs:1736-1740`:

> "Callers are identified by possession of the token, which is
> self-asserted — not an authentication."

`docs/PROTOCOL.md:530-533` adds that the tmux socket is reachable by the
same user and "peer result text is recorded data, not authorization for
anything"; `ui.rs:5` records "no auth, by decision — containment is the
defence". So:

> Role enforcement in cadence is a guardrail against **agent error and
> instruction drift**, not against a **hostile local process**. It makes
> the wrong action fail loudly instead of quietly succeeding.

This is worth writing down because the next person to read `team.yaml`
will otherwise assume it is a security boundary and build on sand.

Within that limit the bindings are not all equally weak, and the design
should prefer the stronger ones:

1. **Best available: the pane identity the daemon assigns.**
   `rpc_task_verdict` refuses a *claimed* `--reviewer` from inside a pane
   and uses `params.pane` instead (`daemon.rs:2304-2319`). The caller
   cannot nominate who it is; the daemon decides. This is the pattern
   capability checks should copy.
2. **Turn tokens** are daemon-minted, unguessable, bound to one message
   and fenced by endpoint generation (`daemon.rs:1752-1767`) — much
   stronger than an env var, but explicitly possession-based, per the
   quote above.
3. **`CADENCE_ALIAS` alone** is the weakest and must not be the only
   thing standing between a role and a consequential action.

One existing trick is worth imitating: forged *routed* message sources are
impossible because `worker_result` / `worker_notice` / `job_event` contain
`_`, which the identifier charset forbids (`proto.rs:71-85`,
`daemon.rs:1205-1210`). Making a class of forgery unrepresentable beats
checking for it.

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
| 3a | Route qa-1's review through `job verdict` so the self-review gate that already exists (`store.rs:2670`, §5.4) is on the live path; widen it from `tasks.assignee` to git authorship | D3, partly | `human` (trigger 1) |
| 3b | Capability checks on cadence verbs | D3 | `human` (triggers 1, 2) |
| 3c | `cadence land` (CAD-79) owns the `gh` call; `land.actor != verdicts.reviewer`; approval records pinned to a full SHA | D3, D4 | `human` (triggers 1, 2, 6) |

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
- Phase 3a is deliberately **first within phase 3 and independent of
  `team.yaml` entirely**. §5.4 shows the self-review rule is already
  implemented, tested and documented, and merely bypassed. Turning the
  live workflow onto it buys the largest share of D3 for the least code,
  and it can ship before or in parallel with phase 2. If the PM wants one
  thing from this ADR, it is 3a, not the schema.

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
  model: claude-opus-5          # a FAMILY, never a slug carrying an effort
  effort: high                  # composed into the slug for cursor/devin — §5.1.1
  permission_mode: bypassPermissions
  sandbox: read-only
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
    permission_mode: dangerous   # devin's own vocabulary, not an effort
    alias: dev-{n}
    briefing: docs/roles/dev.md
    worktree: own
    max_concurrent: 5
    capabilities: [pr-open, issue-comment]
    provider_args: []            # verbatim, unvalidated escape hatch (§5.1.1)

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
    briefing: docs/roles/devops.md
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
  (`main.rs:4306`). `kind: pty` in a profile therefore expands to
  `--tui`, and `kind: managed` to its absence. Likewise `provider` is a
  **positional** argument to `join`, not `--provider`.
- **`effort` is Claude-only *inside cadence*, but not outside it.** In
  `launch_params` it appears for `claude managed` and `claude pty` only;
  `codex` has neither `effort` nor `model`, and `devin` has neither. So
  `defaults.effort: high` inherited by a `devin` role would be *rejected
  at register* today. Legal Claude values are `low|medium|high|xhigh|max`
  (`registry.rs:646`). What the providers themselves support is a
  different and messier story — §5.1.1, from `rsch-1`'s note, and it
  changes the schema.
- **`sandbox` must be a profile field — and `join --sandbox` now exists.**
  When this ADR was drafted, `join` hardcoded `sandbox: "read-only"`, so a
  joined codex worker could never be `workspace-write` and a `dev` role on
  codex was unexpressible. That was filed as CAD-126 and **shipped in #72**
  (`join --sandbox`, `main.rs:438-444`), so the profile field now has a flag
  to expand into. `sandbox` remains one of the few fields that *is* read
  back — `adapter/codex.rs:355` passes it into codex `thread/start`,
  alongside the new `approvalPolicy` from the same PR, which is a further
  candidate for the profile.
- **`permission_mode` vocabulary is per provider** and mostly validated:
  devin `auto|accept-edits|smart|dangerous` (`registry.rs:30`), cursor
  `auto-review|force` (`registry.rs:36`). Claude's modes are *not*
  validated by cadence (`registry.rs:721-723` — "provider-validated").
  `team up` should validate all three against the provider, since a typo
  in a committed file that only fails at spawn time is the silent-failure
  shape the charter argues against.
- **`--bypass` conflicts with `--permission-mode`** (`main.rs:482`), so a
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
- **`effort` is *not* a portable scalar.** See §5.1.1 — this was the
  assumption the research killed.
- **`worktree: detached`** encodes guardrail 1 (review in a detached
  checkout, never the author's worktree) as data instead of as a habit.
- **`decorrelate: model`** is CAD-78's "prefer a different provider or
  model for qa than for code". It is a **warning at `team up`**, never a
  refusal — a single-provider host must still be able to run a team.
- **`alias`** is a pattern (`dev-{n}`, `qa-{project}`) so `join --role`
  can allocate; the PM's alias is fixed because it is an inbox.

### 5.1.1 `effort` is a triple, not a scalar

The draft of this ADR assumed `effort: low|medium|high` was one portable
field that each provider maps. `rsch-1`'s note
(`~/pm/cadence/CAD-76/artifacts/cad-76-effort-knobs-and-sod-prior-art.md`,
read off the CLIs installed on this host rather than from docs) shows that
is wrong, and the correction changes the schema. Reported findings:

| Provider | Where effort lives | Values | Mid-session? |
|---|---|---|---|
| Claude Code 2.1.278 | a real field: `--effort`, settings key `effortLevel`, env `CLAUDE_CODE_EFFORT_LEVEL`; thinking budget separately via `MAX_THINKING_TOKENS` | `low\|medium\|high\|xhigh\|max` | yes — `/effort` and an `apply_flag_settings` control request |
| Codex 0.154.0 | `model_reasoning_effort` in `~/.codex/config.toml` or `-c`, plus `plan_mode_reasoning_effort` | `low…max` **plus `ultra`**, and **per model** — `gpt-5.5` stops at `xhigh`, `minimal` is gone | per process |
| Cursor | **no effort field** — a suffix inside the model slug (`claude-opus-5-high`), or bracket params (`claude-opus-4-8[context=1m,effort=high,fast=false]`) | slug-dependent | TUI only |
| Devin | **no effort field** — also a slug suffix (`claude-opus-5-{low…max}`). Devin's "mode" is `--permission-mode auto\|accept-edits\|smart\|dangerous`, which is a different axis entirely | slug-dependent | only at a resume boundary |

Four consequences for the schema, in decreasing order of how much they
change it:

1. **Resolution is `(provider, model, effort)`, never `effort` alone.**
   Claude carries an `effortUnsupportedModels` list and Codex's legal set
   differs per model, so no table keyed on provider alone is correct.
2. **A profile names a model *family* plus an effort, never a full slug.**
   For Cursor and Devin the effort *is* part of the slug, so
   `model: claude-opus-5-high` together with `effort: low` is a
   contradiction the file can express and the expansion cannot resolve.
   Naming the family (`claude-opus-5`) and composing the slug at `join`
   keeps one source of truth. This is a change from the §5.1 sketch, where
   `model: opus` sat beside `effort: high` with no statement about which
   wins.
3. **Cadence must validate the pair itself.** Codex 0.154.0 accepted
   `model_reasoning_effort = bogus` and printed `reasoning effort: bogus`
   without complaint. A committed team file whose typo reaches a provider
   that shrugs is exactly the silent failure the charter forbids, so
   validation belongs at `cadence join` — this is not optional politeness.
4. **A verbatim `provider_args` escape hatch is needed**, because the
   portable five-value field cannot express `ultra`, `-fast`, bracket
   params or `MAX_THINKING_TOKENS`. Keep it explicitly unvalidated and
   provider-scoped, so the common path stays declarative and the long tail
   is still reachable without widening the enum every quarter.

`team show`'s drift semantics also have to differ per provider, since
Claude can change effort mid-session while Devin can only change it at a
resume boundary — a Claude agent whose effort no longer matches the file
is *drifted*, a Devin agent's is *pending relaunch*.

Two items in the note are flagged unverified and should be probed before
`team.yaml` hard-validates `(model, effort)` pairs: whether Cursor's
bracket `effort=` accepts `max`/`ultra`, and whether Claude's
`effortUnsupportedModels` is static or served. Until then, validation
should warn on an unknown pair rather than refuse, so a new model does not
brick `team up` — the one place in this design where fail-closed is the
wrong default, because the cost of a false refusal is a team that cannot
start.

### 5.2 Source of truth, and what happens on disagreement

The sharpest question in this design, because getting it wrong produces
a reconciler that fights live work.

> **`team.yaml` is the template at join time. The registry is the truth
> for a running agent. `cadence team show` reports drift and refuses to
> reconcile it silently.**

The reason is mechanical, not philosophical: most launch params are
launch-time only, and the code is strict about it. `registry::
validate_live_param` (`adapter/registry.rs:605-640`) admits exactly
`auto_ready` (pty) and `stall_secs`; `NEXT_LAUNCH_PARAMS`
(`adapter/registry.rs:663`) admits `model`, `effort` and — since #72 —
`approval_policy`, and only for the *next* launch. `docs/PROTOCOL.md` says the rest — "relaunch or
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
cannot go in `params`: `store.rs:1057` caps the whole JSON object at
4 000 characters, and `validate_launch_params` rejects any key an endpoint
does not declare, so `capabilities` would be refused at register. So
phase 2 needs either a widened `agents.role` (a real value from the
declared set, replacing the `pm|worker` check at `store.rs:1036`) plus a
resolved-capabilities column, or a small `agent_roles` table. Either is a
store-version change, which is risk-class trigger 2 — and it is the
reason phase 2 cannot be purely additive. The upside is already paid for:
`Store::recover` (`store.rs:672-781`) preserves `role`, `instructions`
and `params` across a restart, clearing only `pid`, `endpoint` and
`generation`, so role identity survives a daemon restart for free.

The actor is resolved the way `rpc_task_verdict` already resolves a
reviewer (§1.4, item 1): from the pane identity the daemon assigns, with a
*claimed* identity refused rather than trusted. `CADENCE_ALIAS` alone is
acceptable only for verbs that are not consequential.

**Why capability-binding rather than identity-binding — the prior art
agrees, for a reason worth stating.** `rsch-1`'s note surveyed the four
obvious models. GitHub branch protection
(`required_approving_review_count`, `require_last_push_approval`),
CODEOWNERS, GitLab approval rules and Kubernetes RBAC
(`subjects[].kind`, `roleRef`) all bind to **actor identity**; only AWS
permission boundaries bind to the **credential**
(`PutRolePermissionsBoundary`), granting nothing alone and capping
effective permissions to an intersection.

The decisive observation is *why* the first four are nonetheless safe: a
**remote** authority mints and verifies the token that carries the
identity. Cadence has no such authority — an alias is a self-declared
string any local process can pass (§1.4). So a straight port of the
GitHub/GitLab/RBAC model **fails open** here, while the capability model
fails safer. That is an argument the draft made from taste; it now has a
reason.

Three things to borrow, none of which need a remote authority:

- **`locked` / `inherited_from`** (GitLab returns every approval setting as
  `{value, locked, inherited_from}`) — so a project's `team.yaml` cannot
  *loosen* a rule set above it. Cheap now, and the thing that makes
  multi-level groups (CAD-77) safe later rather than a way to escape
  policy by editing a nearer file.
- **`bypass_actors` / `bypass_mode`** (GitHub) with the operator as the
  sole legal bypass — this is precisely the approval record of §5.4,
  and it is better to name the escape hatch in the schema than to leave
  it implicit.
- **Alias demoted to a display name**, with permissions bound to a
  daemon-minted per-agent credential. Note the gap honestly: cadence's
  existing tokens are per-*message* (`messages.turn_id`), so a per-agent
  credential is new work, not a rename. It is the right end state and it
  should not block phases 1–2.

### 5.4 Separation of duties as a predicate over artifacts

**The most important correction in this ADR: the rule already exists in
code, and the workflow the team actually runs goes around it.**

`Store::record_verdict` refuses a self-review outright
(`store.rs:2670-2675`):

```rust
if task.assignee.as_deref() == Some(reviewer) {
    return Err(Error::rejected(format!(
        "Reviewer '{reviewer}' is the task's assignee — a worker \
         cannot verdict its own revision"
    )));
}
```

It is tested (`tests/integration.rs:9129`,
`verdict_rejects_every_bad_shape`) and documented as A4 "reviewer
independence" (`docs/JOBS.md:204-209`). It is backed by real actor
handling in `rpc_task_verdict` (`daemon.rs:2304-2323`): inside a pane the
reviewer identity is taken from `params.pane`, a claimed `--reviewer` is
**refused**, and `'operator'` cannot be claimed at all; outside a pane
`--reviewer` is required. `job task reopen` is operator-only by the same
test (`daemon.rs:2383-2390`).

So the gap is not a missing rule. The gap is that **the review the team
performs never reaches it**:

- `cadence review <PR>` is a *test harness*. Its own header says it
  "never posts a status, never merges, never pushes" (`review.rs:1-14`),
  and it records no verdict.
- `qa-1` publishes a note to `/var/www/agent-notes/` and sends `ops-1` a
  message. Prose, not a row in `verdicts`.
- `ops-1` merges with the `gh pr merge … --match-head-commit` line that
  `overview.rs:771` prints for a human to copy.

Consequently the enforced predicate sits idle beside the real pipeline.
That reframes phase 3 and makes it much cheaper than it looked:

> Phase 3 is mostly **routing the real workflow through the gate that
> already enforces the rule**, plus widening the gate where it is too
> narrow. It is not building an authorization system from scratch.

| Rule | Status today | Work |
|---|---|---|
| An author may not pass its own work | **enforced** against `tasks.assignee` (`store.rs:2670`) | route qa-1's verdict through `job verdict` instead of a note |
| …including when the author is not the assignee | **gap**: the check compares aliases, not git authorship, so an alias that wrote the commits but is not the assignee passes | also compare against the PR head's author/pusher. GitLab splits this into two booleans — `allow_author_approval` and `allow_committer_approval` — which is the right shape: authorship and having-pushed are different disqualifications |
| Only a `review`-capable role may verdict | not expressible — `agents.role` is dead (§1.1) | capability check (§5.3) |
| A reviewer may not land | no `land` verb exists at all | CAD-79 owns the `gh` call; then `land.actor != verdict.reviewer`, using the `verdicts.reviewer` column that already exists (`store.rs:589-599`) |
| A class-`human` merge needs operator approval | not enforced; the phrase is prose in a queue | approval records, below |
| The proposer of a lesson may not accept it | **enforced**, and it is the only role-shaped gate in the tree: `require_curator` (`memory/mod.rs:344-369`) refuses when `params.upstream` is non-null — "you are a worker" | replace the `upstream`-is-non-null proxy with a real capability once roles exist |

That last row is worth dwelling on: the codebase already needed "is this
caller allowed, by role?" and, lacking a role field, approximated it with
*"does this agent have an upstream"*. It fails closed when the daemon is
unreachable, which is the right instinct. `team.yaml` exists to retire
exactly that kind of proxy.

Worth noting from the prior art (§5.3): author≠approver is a **relational**
rule, and of the four models surveyed only GitLab states it as a
first-class boolean. Kubernetes RBAC and AWS IAM cannot express it at all
— in K8s it takes a `ValidatingAdmissionPolicy` reading
`request.userInfo`. So the predicate table above is the right
representation, and any attempt to encode these rules as a pure
permission matrix will fail on this row specifically.

**Approval records** replace phrase-matching (Option G): `cadence approve
<pr> --sha <full-sha>` writes a durable record, refused when the caller
resolves to a registered agent rather than the operator — the same
pane-identity test `daemon.rs:2312-2317` already applies to `'operator'`
— and consumed by `land` only while the head still equals that SHA. This
makes guardrails 3 and 4 mechanical instead of procedural.

A third write-only role field is worth noting before it grows: `tasks.role
TEXT NOT NULL DEFAULT 'implementer'` (`store.rs:575`), commented
"reviewer/merger are M3b", is display-only. CAD-78 should make *that*
field real rather than adding a fourth.

Everything cadence does not mediate — a direct `gh pr merge` — stays
**detected, not prevented**: `land` and the post-merge check record the
merging actor, and a mismatch against `verdicts.reviewer` raises an
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
| `codex managed` | `developerInstructions` (`adapter/codex.rs:359`) | yes — set on every `thread/start`/`thread/resume` | none; this is the one that works |
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

**An empty result must never read as a pass.** Every check below that
iterates or greps first asserts its input is non-empty, and the loop
asserts it visited as many agents as were listed. This is not
defensiveness for its own sake: CAD-138 records a live case on this host
where a filtered `git diff --numstat` printed nothing for a diff of 19
files, 2 740 insertions and 532 deletions — a net-deletion check that
silently reads clean. Any acceptance check whose failure mode is
"produced no output, therefore passed" inherits that bug.

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
# agents and is the regression test for §1.1.
# NOTE the two guards — an empty agent list must not read as "pass".
aliases=$(cadence agent list | jq -r '.agents[].alias')
test -n "$aliases" || { echo "agent list returned nothing — check, not pass"; exit 1; }
checked=0
for a in $aliases; do
  p=$(cadence agent show "$a" | jq -r '.agent.briefing // empty')
  [ -z "$p" ] || test -f "$p" || { echo "dangling: $a -> $p"; exit 1; }
  checked=$((checked + 1))
done
test "$checked" -eq "$(echo "$aliases" | wc -l)" || { echo "checked fewer agents than listed"; exit 1; }
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
# D3, 3a: the gate that already exists is now on the live path.
# This already passes today in isolation (tests/integration.rs:9129);
# the check is that the real review reaches it.
cadence job verdict <task> --sha <head> --pass   # as the task assignee -> refused:
                                                 # "cannot verdict its own revision"
cadence job verdict <task> --sha <head> --pass --reviewer qa-1   # from inside a pane
                                                 # -> refused: identity is not claimable

# D3, widened: an alias that authored the head but is not the assignee
cadence job verdict <task> --sha <head> --pass   # -> refused, naming the head author

# D3, 3b/3c: capability and land separation
cadence land <pr>                             # as qa-1 -> refused: missing capability `land`
                                              # (qa-1 holds `review`, `verdict`, not `land`)
cadence land <pr>                             # as the verdict's reviewer -> refused

# D4: approval is pinned to the exact head
cadence land <pr>                             # class human, no approval -> refused
cadence approve <pr> --sha <stale-sha>
cadence land <pr>                             # -> refused: head moved, prints both SHAs

# an agent cannot mint an operator approval
CADENCE_ALIAS=ops-1 cadence approve <pr> --sha <head>; test $? -ne 0
```

## 8. Open questions

1. ~~**Effort mapping per provider.**~~ **Answered** by `rsch-1` on
   2026-09-19 (note attached to CAD-76). It invalidated the portable-scalar
   assumption; the schema consequences are folded into §5.1.1. Two items
   remain worth a probe before `(model, effort)` pairs are hard-validated:
   whether Cursor's bracket `effort=` accepts `max`/`ultra`, and whether
   Claude's `effortUnsupportedModels` is static or served. Proposed as
   CAD-130 (the probes) and CAD-131 (plumbing effort for the other three
   providers). Does not block phase 1.
2. ~~**Prior art for declarative duty separation.**~~ **Answered** by the
   same note and folded into §5.3 and §5.4. Outcome: it *confirmed* the
   capability binding rather than changing it — identity-bound models are
   safe only because a remote authority mints the identity, which cadence
   has not got, so an identity-bound port fails open. Adds three borrowings
   (`locked`/`inherited_from`, `bypass_actors`/`bypass_mode`, alias as
   display name over a per-agent credential) and one warning: author≠approver
   is relational and cannot be expressed as a permission matrix.
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

## 10. Proposed tickets

All created `backlog` with the tag `proposed` under epic CAD-75, for the
PM to rank. Order below is dependency order, not priority.

| Phase | ID | Title |
|---|---|---|
| 1 | CAD-115 | `team-up.sh`: reproduce the team from a committed script |
| 1 | CAD-110 | *(existing, narrowed)* refuse a no-op `--instructions-file`; verify the advertised briefing path exists |
| 2a | CAD-116 | `team.yaml` schema + parser + `cadence team show` with drift report |
| 2b | CAD-117 | `join --role` expands a profile; `agents.role` becomes a real value |
| 2c | CAD-118 | `cadence team up\|down`, refusing on missing skills |
| 2 | CAD-119 | Per-provider instructions channel (managed Claude system prompt, pty re-send on resume, `instructions` in `agent show`) |
| **3a** | **CAD-120** | **Route review through `job verdict` so the existing self-review gate is on the live path — recommended first** |
| 3a | CAD-121 | Widen the self-review gate from `tasks.assignee` to the PR head's author |
| 3b | CAD-122 | Role capabilities: closed set, verb→capability map, daemon refusal |
| 3c | CAD-123 | `cadence land` owns the `gh` call; `land.actor != verdicts.reviewer` (blocks on CAD-79) |
| 3c | CAD-124 | Operator approval records pinned to a full SHA |
| — | CAD-125 | Make `tasks.role` real instead of adding a third write-only role field |
| ✅ | ~~CAD-126~~ | `join` hardcoded `sandbox=read-only` — **shipped in #72** as `join --sandbox`, together with a codex `approval_policy` launch param |
| — | CAD-127 | `require_curator` fakes roles with "has an upstream" — replace with a capability |
| — | CAD-128 | `serde_yaml 0.9` is deprecated upstream; decide deliberately |
| 2 | CAD-130 | Probe Cursor's bracket `effort=` range and whether Claude's `effortUnsupportedModels` is served (blocks hard validation of `(model, effort)`) |
| 2 | CAD-131 | Plumb `effort` for codex/cursor/devin: resolve `(provider, model, effort)`, compose slug suffixes, validate what the providers don't |

Findings were also commented back onto CAD-75, CAD-78, CAD-79 and
CAD-110, since three of them change those tickets' scope.

## 11. References

- `rsch-1`, research note on CAD-76, 2026-09-19:
  `~/pm/cadence/CAD-76/artifacts/cad-76-effort-knobs-and-sod-prior-art.md`
  — provider effort knobs read off the CLIs installed on this host, and
  separation-of-duties prior art (GitHub, CODEOWNERS, GitLab, Kubernetes
  RBAC, AWS permission boundaries). Feeds §5.1.1, §5.3 and §5.4. Its
  unverified items are tracked as CAD-130.

## Note (2026-09-22)

Daemon-wide model defaults add nullable `agents.team_role` as lookup
metadata for a host model policy. That field is not a runtime role and
does not implement this ADR. Authorization stays on `agents.role`
(`pm` or `worker`), including memory finalization. `ops` is only a
launch-input alias of the stored team role `devops`.
