# 0003 — Per-project worktree policy, and adopting a worktree we did not create

- Status: **proposed**
- Date: 2026-09-20
- Author: `arch-1` (architect)
- Deciders: operator (whether an external root is supported at all), PM (ticket order, coordination with `aos-pm`)
- Issues: CAD-142 (this), relates AOS-9, CAD-94 (the finish guards this must not break), CAD-91 (resource hygiene)
- Code citations pinned to `33a6a82`. Line numbers move; re-locate by symbol.

## 1. Context

The AgenticOS team ran cadence against their own stack and could not use
`cadence dispatch` at all. Their policy puts worktrees at
`/home/ubuntu/Project/agenticos-stack/worktrees/<repo>-<task>` based on
`origin/staging`; cadence creates them at `<repo>/.cadence/wt/<name>` on
`cadence/<name>` from `origin/HEAD`. So they created worktrees by hand,
launched with `--cwd`, and recorded refs through the CLI — a workaround
the report is careful to describe as an integration gap, not a defect.

This is not a hypothetical. **Their stack is on this host**, and it is
worth reading before designing anything:

| Fact | Evidence |
|---|---|
| Real linked worktrees at an external root | `/home/ubuntu/Project/agenticos-stack/worktrees/` holds `agenticos-v2-auth`, `agenticos-v2-bootstrap`, `agenticos-v2-appflow`, `agenticos-v2-workspace`, `api-agent-container-model` |
| All of one repo | `git rev-parse --git-common-dir` → `/home/ubuntu/Project/agenticos-stack/agenticos-v2/.git` |
| The root is a **sibling of the repo**, not inside it | `…/agenticos-stack/worktrees/` beside `…/agenticos-stack/agenticos-v2/` |
| Branch names are theirs, not ours | `backend/email-otp`, `bootstrap/docs-ci` — slash-namespaced, no `cadence/` prefix |
| Base really is `origin/staging` | `agenticos-v2-bootstrap` tracks `origin/staging` |
| **Directory name ≠ branch name** | dir `agenticos-v2-auth` sits on branch `backend/email-otp` |

### 1.1 The assumption that has to give

`src/worktree.rs:1-3` states the current model in its own header: both
the standalone and issue paths "share the same layout
(`<root>/.cadence/wt/<name>` on `cadence/<name>`)". One `<name>` drives
*both* the directory and the branch.

The last row of the table above breaks that. AgenticOS names the
directory after the repo and the task, and the branch after the area and
the task. A single `<name>` cannot express both. So per-project policy
needs **two independent templates**, not one — this is the part
cookie-cesium's triage sketch (which is otherwise the right shape, see
§3) does not cover, and it is the difference between "configurable" and
"actually usable by the team that asked".

It is worse than an ergonomic gap. `finish` pairs a worktree with its
branch by **recomputing the name**:

```rust
open_branch(Some(&format!("cadence/{wt_name}")))   // issue/finish.rs:277
```

where `wt_name` is the worktree directory's own `file_name()`. Adopt
`agenticos-v2-auth` sitting on `backend/email-otp` and that lookup finds
nothing: the issue finishes with `branch = ""`, **the branch is never
cleaned, and the `--merged` sweep skips it as "no branch ref"**. A
silent, permanent branch leak, produced by the happy path. Any adoption
that does not fix the pairing ships that leak on day one.

Two more hardcoded name assumptions sit outside the eight files:

- `overview.rs:652-662` decides "this issue in review has an open PR" by
  testing `open_pr_branches.starts_with("cadence/<id-lower>-")`. A
  project-chosen branch template silently loses its PR association.
- `issue/dispatch.rs:139-141` **independently recomputes** the worktree path to
  build the kickoff body, with a comment warning that it must stay in
  step with `start`. Two predictions of one layout is one too many.

### 1.2 Configurable root is not a one-file change

`.cadence/wt` is assumed in **eight modules**, not one:

```
src/worktree.rs  src/session.rs  src/main.rs  src/issue/start.rs
src/issue/cli.rs src/issue/finish.rs  src/review.rs  src/doctor/host.rs
```

Two of those matter more than the rest, and both are recent:

- **`src/session.rs` (#63, merged today).** `session end` scans
  `.cadence/wt/*` under each repo checkout to find orphan worktrees —
  "`.cadence/wt/*` with neither an open PR nor an open issue"
  (`session.rs:1118`), reporting `orphan worktree {root}/.cadence/wt/{name}`
  (`session.rs:1127`). A worktree at a configured external root is
  **invisible to this scan**. Make the root configurable without
  teaching session.rs about it and every AgenticOS worktree becomes an
  untracked leak — precisely the class of failure CAD-91 exists to stop,
  reintroduced by the feature meant to help.
- **`src/worktree.rs` `shared_target_dir` (#69).** The shared cargo cache
  is `<repo-root>/.cadence/target/shared`, documented as "one
  dependency-artifact store for every `.cadence/wt` lane". An external
  lane can still point at the repo's shared cache — the repo root is
  still knowable — but the *invariant* ("every lane lives under this
  repo") is what the code leans on, and it needs restating rather than
  quietly violating.

`src/doctor/host.rs` also reports stale worktrees, so it inherits the
same blindness.

So the honest framing: **CAD-142 is not "add a `--worktree-root` flag".
It is "stop hardcoding a layout in eight places, and keep every consumer
of that layout correct."** A flag alone would ship a silent leak.

### 1.3 What CAD-94 established that must survive

`issue finish` guards a worktree per worktree, not per agent: it blocks
on a live message bound to that worktree, scans the pane tree and
`/proc`, refuses on a dirty tree, and only removes a checkout whose work
is survivable (ancestry, patch-equivalence, squash merge, merged PR, or
pushed). `--force` overrides each and records a `Forced:` trailer.

Adoption is the risky verb here, because it binds cadence's lifecycle to
a directory **cadence did not create and does not know the history of**.
Reading the guards closely turns up four specific ways a naive adoption
destroys an operator's tree or leaks state. These, not the general
principle, are what §5.3 has to answer:

1. **Start's rollback is a demolition.** If the tracker commit is
   refused, `start` runs `git worktree remove --force` **and**
   `git branch -D` (`issue/start.rs:405-413`, and the cargo-failure path at
   `issue/start.rs:339-346`). The comment says "the worktree, the branch and the
   tracker commit stand or fall together" — correct for a tree cadence
   just made, catastrophic for one it adopted. An adopted tree must be
   exempt from both legs.
2. **There is no provenance marker.** `finish` removes whatever the
   `worktree` ref points at; none of its guards ask who created it. The
   codebase already has the pattern for this — `review.rs:876` drops a
   `.cadence-review-tree` marker, and `session.rs:801-803` skips marked
   trees — but issue worktrees carry nothing.
3. **The repo-root fallback counts path components.** When the dir is
   gone, `finish` recovers the repo as
   `parent()?.parent()?.parent()?` (`issue/finish.rs:300-311`) — exactly the
   depth of `<root>/.cadence/wt/<name>`. A configurable root breaks this
   the moment a worktree goes missing, which is precisely when you need
   it to work.
4. **`configure_cargo_target` mutates the tree on every start**,
   including the idempotent re-attach (`issue/start.rs:271`). Adopting a tree
   that already has a non-cadence `target/` merges its hashed artifact
   dirs into the shared cache (`worktree.rs:415-418`) unless the project
   sets `build.target_dir = "per-worktree"`.

## 2. What "done" looks like

- **D1** A project can declare a worktree root, a base ref, a directory
  name template and a branch name template; `issue start` and `dispatch`
  honour them; omitting the config changes nothing for existing projects.
- **D2** `issue start --adopt <dir>` binds an existing worktree
  idempotently, recording the branch it is *actually* on.
- **D3** Adoption refuses: a dir that is not a worktree of the project's
  repo; one already bound to another issue; one owned by another agent;
  a mismatched branch. Each refusal names what it saw and what it wanted.
- **D4** One worktree per PR still holds — adoption never creates a
  second checkout.
- **D5** `finish` removes an adopted checkout only through the unchanged
  CAD-94 gates, and **never** removes a directory cadence did not create
  unless adoption recorded it.
- **D6** Every existing consumer of the layout — `session end` orphan
  detection, `doctor --host` stale-worktree reporting, the shared target
  cache — still sees worktrees at a configured root.

D6 is the one a naive implementation drops, so it is stated as an
acceptance criterion rather than a footnote.

## 3. Options

### Option A — Do nothing

AgenticOS keeps the manual workaround: hand-made worktree, `--cwd`,
refs recorded via the CLI.

- D1–D6 ✗.
- It demonstrably works — they shipped with it, and they were careful not
  to create a prohibited checkout to reproduce. The real cost is that
  `dispatch` is unusable for them, so every kickoff is manual, and the
  tracker learns about the worktree only if a human remembers to record
  the refs. That is exactly the "evidence over self-report" gap the
  charter names, reintroduced by a config mismatch.
- The sharper cost is that **`--cwd` is entirely unvalidated**. It is
  taken as given (`main.rs:4341-4344`) and the daemon checks only that
  the directory exists and canonicalises (`daemon.rs:1157-1163`). Nothing
  ties `agent.cwd` to any issue, worktree ref or branch. So an agent
  working in a hand-made tree has **no recorded relationship to the work
  it is doing**, and the finish guards can only catch it incidentally,
  through the `/proc` cwd scan, and only while a process is standing in
  the tree at that moment. The workaround does not merely bypass
  dispatch; it bypasses the ownership model that CAD-94 exists to
  enforce.
- Rejected, but it sets the bar: A is survivable, so this work does not
  justify weakening any CAD-94 guard to go faster.

### Option B — The smallest thing that could work: adopt only

Ship `--adopt <dir>` and nothing else. No policy config. Teams that need
a different layout create the worktree themselves — which they are
already doing — and adoption binds it so `dispatch`, refs, ownership and
`finish` all work normally.

- D2 ✓ D3 ✓ D4 ✓ D5 ✓; D1 ✗ D6 partially (adopted dirs still need to be
  visible to session/doctor, but there is exactly one code path creating
  that visibility gap instead of a general one).
- This is genuinely attractive. It removes the blocker with one new flag
  and no cross-cutting change, because **the team is already creating the
  worktrees**; policy config would only automate a step they have
  automated themselves. It also sidesteps §1.1 entirely — an adopted dir
  brings its own name and branch, so no template language is needed.
- Weakness: the layout stays hardcoded for everyone who *would* like
  cadence to create it correctly, and "adopt" becomes the permanent
  answer to "support my layout", which is a workaround promoted to an
  API.

### Option C — Policy config only, no adoption

Per-project root/base/name templates; `start` and `dispatch` create
worktrees in the right place. No `--adopt`.

- D1 ✓ D6 (if done properly) ✓; D2–D5 ✗.
- Weakness that kills it as a standalone: it does not help a team that
  **already has** worktrees, which is AgenticOS's actual state today.
  They would have to tear down and recreate live lanes to adopt cadence's
  creation path — the most dangerous possible migration, and one that
  runs straight into the finish guards.

### Option D — Policy config plus adoption (cookie-cesium's shape, extended)

Both, with the layout centralised so every consumer stays correct.
cookie-cesium's triage comment on CAD-142 already sketches this well:
project-level root with a `--worktree-root` override; `--adopt <dir>`
validating repo membership, recording the actual branch, refusing when
bound elsewhere or foreign-owned; finish unchanged; one-worktree-per-PR
preserved by binding rather than creating.

I adopt that shape and add three things it does not cover:

1. **Base ref policy** — AgenticOS needs `origin/staging`. `--base`
   exists as a flag but there is no per-project default, so every
   dispatch would have to remember it. A forgotten `--base` silently
   branches from the wrong place, which is a correctness bug, not an
   ergonomic one.
2. **Two name templates** (§1.1), because dir name and branch name are
   independent in the real layout.
3. **D6** — teaching `session.rs`, `doctor/host.rs` and the shared-target
   logic about the configured root, so the feature does not create the
   leak described in §1.2.

- D1–D6 ✓. Cost: touches eight modules plus the tracker's project schema.

### Option E — Rejected: make the agent's `--cwd` authoritative

Drop worktree management; let each team create checkouts and point
cadence at them with `--cwd`, recording refs manually.

Rejected: this is Option A renamed, and it moves the one-worktree-per-PR
and finish-safety invariants out of cadence and into each team's
discipline. CAD-94 exists because that discipline failed *here*, with
our own team, twice (a busy worker's worktree removed).

### Option F — Rejected: a general path-template language

A full template DSL (`{repo}`, `{issue}`, `{slug}`, `{area}`, conditionals)
for roots, dirs and branches.

Rejected as over-built for two known layouts. Two fixed-vocabulary
templates with a small documented placeholder set cover both, and a
placeholder set is reviewable where a DSL is not. If a third layout
arrives that needs conditionals, that is the evidence to revisit.

## 4. Decision

**Adopt Option D, in two phases, with Option B as phase 1.**

| Phase | Content | Satisfies | Risk class |
|---|---|---|---|
| 1 | `issue start --adopt <dir>` + the dispatch equivalent; validation and refusals; adopted dirs recorded so `session end` and `doctor --host` can see them | D2–D5, part of D6 | `human` — it writes tracker refs and feeds the deletion path (triggers 2) |
| 2 | Project-level `worktree:` policy (root, base, dir template, branch template) + `--worktree-root`/`--base` overrides; centralise the layout so all eight consumers read one function | D1, D6 | `human` (trigger 2 via finish/session paths); the schema part alone is `auto` |

Phase 1 first because it unblocks AgenticOS **this week** against the
worktrees they already have, and because it is the phase whose absence
forces them to bypass dispatch. Phase 2 is the general fix and is worth
doing properly rather than quickly, since §1.2 shows a careless version
ships a resource leak.

A note on sequencing: phase 2 should land the **layout centralisation
before** the config that varies it. Introducing a configurable root while
eight modules still compute the path independently is how D6 gets missed.
Centralise first, vary second — the refactor is behaviour-preserving and
reviewable on its own.

## 5. Design

### 5.1 Project policy

Extends `project.yaml` (`src/issue/project.rs`), which already carries
`key`, `prefix`, `repos`, `components`, `tags`, `default_owner` and
`build`. All keys optional; absent means today's behaviour exactly.

`build` is the shape to copy, because it is the **only existing
per-project policy field** (`issue/project.rs:24-28`): an `Option<Table>` of
all-`Option` fields, `#[serde(default, skip_serializing_if)]`, validated
at the point of consumption with a rejection naming the project. Copy its
ergonomics too — and note the one thing to fix rather than copy: there is
no CLI surface for it (`write::project_add` hardcodes `build: None`,
`issue/write.rs:285`), so it is hand-edited.

```yaml
worktree:
  root: /home/ubuntu/Project/agenticos-stack/worktrees   # default: <repo>/.cadence/wt
  base: origin/staging                                   # default: origin/HEAD, else current branch
  dir: "{repo}-{task}"                                   # default: "{id-lower}-{slug}"
  branch: "{area}/{task}"                                # default: "cadence/{id-lower}-{slug}"
```

Placeholders are a closed set — `{repo}`, `{id}`, `{id-lower}`, `{slug}`,
`{task}`, `{area}` — validated at parse. An unknown placeholder is a
refusal, not a literal: a typo that silently becomes a directory called
`{taks}` is the kind of thing nobody notices until `finish` cannot find
it.

**A misspelled *key* is the more dangerous case, and today it fails
open.** `Project` does not set `deny_unknown_fields`, so
`serde_yaml::from_str` silently discards anything it does not recognise
(`issue/project.rs:142-149`). Write `worktrees:` instead of `worktree:` and the
policy is ignored without a word — worktrees quietly land in
`.cadence/wt` and the team concludes cadence does not honour their
config. Since this ADR is adding the first policy table anyone will
hand-edit, it should also add the guard: reject unknown top-level keys in
`project.yaml`, or at minimum warn on them. Fail closed, charter
principle 3.

`root` may be absolute or relative to the repo. When it is **outside the
repo**, `ensure_cadence_ignored` is skipped (there is nothing to ignore)
— today it is called unconditionally, and that is correct only for the
in-repo default.

### 5.2 Adoption

```
cadence issue start <ID> --adopt <dir>
cadence dispatch <ID> --to <agent> --adopt <dir>
```

Adoption **binds**, never creates. Sequence, all checks before any write:

1. `<dir>` is a git worktree, and its `--git-common-dir` resolves to one
   of the project's declared repos. Otherwise refuse, naming both paths.
2. Read the branch it is *actually* on and record that — never assume
   `cadence/<name>`. AgenticOS's `agenticos-v2-auth` is on
   `backend/email-otp`; an adoption path that writes a computed branch
   ref would record a branch that does not exist.
3. Refuse when the dir is already bound to another issue (scan recorded
   `worktree` refs), or when the issue already has a live worktree ref —
   that is the one-worktree-per-PR invariant (D4).
4. Refuse when the dir is owned by another agent, using the same
   owner/busy test `finish` uses, so ownership has one definition.
5. Record `worktree` and `branch` refs exactly as `issue start` does, plus
   **`adopted: true`** and **the repo root** (see §5.3 hazard 3), and set
   the issue owner if unset.
6. Idempotent: adopting the same dir for the same issue again is a no-op
   that re-prints the refs, not an error.

Two existing refusals in `start` are the ones adoption **replaces rather
than deletes** (`issue/start.rs:313-328`): "Branch already exists but the issue
records worktree X" and "Worktree dir already exists but the issue records
branch Y". Today they are dead ends whose only advice is to pick another
`--name`; with `--adopt` they become the signpost to the supported path.
The same is true of the error string in `worktree.rs:76-83` — "reuse it
with `--cwd {dir}`" — which is the closest thing to adoption that exists
today, and it is advice in an error message rather than a code path.

**The branch ref must be paired by value, not recomputed.** §1.1 shows
`issue/finish.rs:277` rebuilds `cadence/{dirname}` to find the branch. Adoption
records the real branch, so `finish` must look the branch ref up **by the
recorded value**, exactly as it already closes refs by value
(`issue/finish.rs:1318-1320`). This is a small change to one lookup and it is
the difference between adoption working and adoption leaking a branch
every time.

### 5.3 Why `adopted: true` is load-bearing, not bookkeeping

This is the safety crux of the whole ADR.

`finish` removes worktrees. Today every worktree it can see, cadence
created, at a path it computed. Adoption breaks that: cadence would now
be able to delete a directory a human or another tool made, possibly
containing work no cadence record knows about.

So the marker drives a **stricter** rule, not a looser one:

- `finish` runs the **unchanged** CAD-94 gates on an adopted checkout —
  in-use, dirty, survivability, pane/proc scans. Nothing is relaxed.
- Additionally, `finish` removes an adopted directory **only** when the
  adoption record exists and still matches the dir's current repo and
  branch. If the branch moved under us, refuse and say so: the thing we
  adopted is not the thing we are about to delete.
- `--force` does **not** extend to deleting an unrecorded directory.
  Forcing past a busy guard on a checkout we adopted is one decision;
  deleting an arbitrary path is another, and the second is not in scope
  for this verb.

And each of §1.3's four hazards gets an explicit answer:

| Hazard | Answer |
|---|---|
| 1. Start's rollback demolishes (`worktree remove --force` + `branch -D`) | The rollback is **skipped entirely** for an adopted tree. Adoption creates nothing, so there is nothing to roll back; restoring the tracker front is the whole undo. This must be a branch in the code, not a comment. |
| 2. No provenance marker | Adoption writes a marker in the tree, following the existing `.cadence-review-tree` precedent (`review.rs:876`, skipped by `session.rs:801-803`). It records the issue id and that cadence did not create this tree. `finish` cross-checks it against the ref; `session`/`doctor` read it instead of guessing from the path. |
| 3. Repo root recovered by counting three parents (`issue/finish.rs:300-311`) | The `worktree` ref records the repo root at adoption, and `finish` prefers the recorded value, falling back to the parent walk only for legacy refs. Path depth stops being load-bearing. |
| 4. `configure_cargo_target` merges a foreign `target/` into the shared cache | Adoption **does not** call it. An adopted tree keeps its own `target/`; opting into the shared farm is a separate, explicit act. It already refuses on foreign symlinks (`worktree.rs:407-412`), but not running it at all is the safer default for a tree we did not build. |

The marker deserves one note: it is **evidence, not permission**. A tree
with a marker but no matching ref is an orphan to report, not a thing to
delete — the same asymmetry §5.3 applies everywhere.

Put plainly: **adoption grants cadence the right to manage a checkout, not
the right to delete anything it is pointed at.**

### 5.4 Keeping the other consumers correct (D6)

One function computes worktree locations for a project; every consumer
calls it instead of joining `.cadence/wt` itself:

| Consumer | Today | After |
|---|---|---|
| `worktree::create_worktree` | joins `.cadence/wt/<name>`, branch `cadence/<name>`, base literal `"HEAD"` (`worktree.rs:64-93`) | asks policy |
| `issue/start.rs:220` | joins the path itself | asks policy |
| `issue/dispatch.rs:136-147` | **re-predicts** the path for the kickoff body | asks policy — one predictor, not two |
| `issue/finish.rs:277` | recomputes `cadence/{dirname}` to pair the branch | pairs **by recorded value** (§5.2) |
| `issue/finish.rs:300-311` | recovers the repo by three `parent()` calls | prefers the recorded repo root |
| `session.rs` orphan scan | globs `<repo>/.cadence/wt/*` | globs policy roots **plus** recorded adopted dirs |
| `doctor/host.rs` stale/size rows | same glob | same |
| `overview.rs:652-662` | matches PRs by `cadence/<id-lower>-` prefix | asks policy for the branch template |
| `review.rs` | detached checkout paths, `.cadence-review-tree` marker | unchanged; it is the marker precedent §5.3 copies |
| `shared_target_dir` | `<repo>/.cadence/target/shared` | unchanged — keyed to the repo, which an external lane still knows |

Adopted dirs need explicit enumeration because no glob will find them:
they are wherever the adopter said. The tracker's `worktree` refs are the
index, which is another reason step 5 of §5.2 records them.

## 6. Consequences

**Good.** AgenticOS can use `dispatch`, so their work becomes visible to
the tracker and the review loop instead of living in a human's memory.
The layout stops being eight independent string joins. Adoption is a
general answer to "cadence did not make this checkout", which will recur.

**Bad, accepted.** A configurable root means the answer to "where are my
worktrees" is no longer universal, and every future consumer of the
layout must remember to ask. The centralisation in §5.4 is what keeps
that from being a recurring bug, and it is the part most likely to be
skipped under time pressure.

**Ugly.** Adoption puts a directory cadence did not create on the path to
a deletion verb. §5.3 is the mitigation and it should be reviewed harder
than the rest of this ADR.

## 7. Acceptance checks

Commands `qa-1` runs. An empty result must never read as a pass — assert
inputs are non-empty before asserting properties (CAD-138).

**Phase 1 — adoption**

```bash
# D2: binds an existing worktree and records the branch it is really on
cadence issue start CAD-XX --adopt /home/ubuntu/Project/agenticos-stack/worktrees/agenticos-v2-auth
cadence issue show CAD-XX | grep -q 'backend/email-otp'   # the ACTUAL branch, not cadence/<name>

# idempotent
cadence issue start CAD-XX --adopt <same-dir>; test $? -eq 0

# D3: each refusal, and each must name what it saw
cadence issue start CAD-YY --adopt /tmp/not-a-worktree        ; test $? -ne 0
cadence issue start CAD-YY --adopt <dir-of-another-repo>      ; test $? -ne 0
cadence issue start CAD-YY --adopt <dir-already-bound-to-XX>  ; test $? -ne 0
cadence issue start CAD-YY --adopt <dir-owned-by-other-agent> ; test $? -ne 0

# D4: no second checkout was created
test "$(git -C <repo> worktree list | wc -l)" -eq "$before"

# D5: finish still refuses a busy/dirty adopted checkout
touch <adopted-dir>/scratch && cadence issue finish CAD-XX ; test $? -ne 0
# and refuses when the branch moved out from under the adoption record
git -C <adopted-dir> checkout -b other && cadence issue finish CAD-XX ; test $? -ne 0

# the branch-leak regression (§1.1): a dir/branch mismatch must still
# pair, finish and clean the branch — this is what fails today
cadence issue finish CAD-XX --dry-run | jq -e '.branch == "backend/email-otp"'
cadence issue finish CAD-XX --merged --dry-run | grep -qv 'no branch ref'

# hazard 1: a refused tracker commit must NOT demolish an adopted tree
CADENCE_FORCE_COMMIT_FAIL=1 cadence issue start CAD-YY --adopt <dir> ; test $? -ne 0
test -d <dir>                                    # still there
git -C <repo> rev-parse --verify refs/heads/backend/email-otp   # branch still there

# hazard 4: an adopted tree keeps its own target/, unmerged into the farm
test ! -L <adopted-dir>/target/debug/deps
```

**Phase 2 — policy**

```bash
# D1: honoured, and absent config changes nothing
cadence issue start CAD-ZZ            # with worktree: config -> lands under the configured root
cadence issue start CAD-ZZ            # in a project with no worktree: key -> .cadence/wt as before

# base policy is applied without an explicit --base
git -C <new-worktree> rev-parse --abbrev-ref '@{u}' | grep -q staging

# unknown placeholder refuses rather than creating a literal directory
printf 'worktree:\n  dir: "{taks}"\n' >> /tmp/bad-project.yaml
cadence issue start CAD-ZZ --project-file /tmp/bad-project.yaml ; test $? -ne 0

# D6: the leak check — a worktree at a configured root is still seen
roots=$(cadence session end --dry-run | grep -c 'orphan worktree')
test -n "$roots"     # and an orphan at the configured root appears in it
cadence doctor --host | grep -q '<configured-root>'
```

## 8. Open questions

1. **Operator:** is an external worktree root supported at all, or only
   roots inside the repo? Everything in §5 assumes external is allowed,
   because that is AgenticOS's actual policy — but it is the decision
   that makes `.cadence/`-ignoring conditional and removes the
   "everything under one repo" invariant.
2. **PM / `aos-pm`:** does AgenticOS want cadence to *create* worktrees
   to their policy (phase 2), or is adoption (phase 1) sufficient
   permanently? If adoption is enough, phase 2 is speculative and should
   wait for a second team with a different layout.
3. Does `--adopt` belong on `dispatch` as well as `start`, or should
   dispatch always adopt-or-create via policy? Affects how many call
   sites need the validation in §5.2.
4. Queue position: CAD-142 is P2/ready behind PR #68 and shares files
   with CAD-95 (board-dev, `doing`). The centralisation in §5.4 touches
   `issue/*`, so it should be sequenced against CAD-95 rather than
   racing it.

## 9. How we would know this was wrong

- Nobody but AgenticOS ever sets `worktree:` → phase 2 was speculative
  and adoption was the whole answer (Option B was right).
- An adopted checkout gets deleted with work in it → §5.3 was too weak,
  and adoption should not grant deletion rights at all; `finish` should
  unbind and leave the directory.
- Orphan worktrees start appearing at configured roots → D6 was skipped
  and §1.2's warning came true.
- The two name templates are always set to the same value → dir and
  branch were not independent after all, and one `{name}` would do.
