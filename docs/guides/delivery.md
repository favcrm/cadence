# Delivery

How work gets from an idea to done in Cadence, for any team: software,
design, business or marketing. This page is the single product reference for the
lifecycle, the delivery policy, the evidence each review takes, what the human
approval does and the refusals you can meet. Other pages link here instead of
restating it.

It describes what the product does today. A hosted workspace runs the server
half of the gate; a native install runs it in the local daemon. The differences
are in [Local and hosted](#local-and-hosted).

## The lifecycle

Every issue follows one path:

| Step | What happens | Status |
| --- | --- | --- |
| Draft | An issue is written with its outcome and acceptance. | `backlog`, then `ready` |
| Start | An agent or person claims it and gets an isolated lane (a git worktree for code). | `doing` |
| Review | The work is checked against the acceptance and the project's policy. A human approves. | `review` |
| Delivery | The approved work lands (code) or is shared (a document). | `done` |

Agents do the work and the checks. A human approves at review. Delivery follows
the approval. An agent never records the human's approval.

For software, delivery is a pull request that the merge queue lands after the
gate passes. For a document it is the owner sharing the approved version; see
[Document review](#document-review-for-non-code-work).

## Policy: each project owns its rules

A project declares its delivery rules as data, so the platform ships no
repository's paths and no team's rules.

- **The `delivery:` section** of the project's `PROJECT.md` sets the merge
  method, the review names, the risk rules and the size limits. The schema is in
  [BOARD.md](../BOARD.md#projectmd-delivery--the-per-project-delivery-policy). It
  takes effect once the owner approves it. Without a section the default policy
  applies: one general agent review, squash merge through the queue. What the
  hosted gate counts today is described under
  [Trusted lists](#trusted-lists).
- **The repository's trusted lists** say which paths need what. They live in the
  repository, in `docs/roles/risk-paths.toml`, `docs/roles/one-review-paths.toml`
  and, for review workflows, `docs/roles/workflow-auto-paths.toml` and
  `docs/roles/review-workflow-principals.toml`.

On a hosted org the owner records a project's repositories and its `delivery:`
section with `issue project delivery` (needs a login made with
`--audit-approve`):

```sh
cadence --org acme issue project delivery <project-key> \
  --repo <owner>/<repo> --delivery-file delivery.yaml
```

The gate refuses a project with no recorded repository.

### Trusted lists

The lists are read from the pull request's anchor, never from the pull request
itself, so a change cannot weaken its own rules. The anchor is the default
branch's live tip for a PR based on it, otherwise the PR's base. A hosted board
resolves the default branch as `main` literally, and reads the review-workflow
principals and `workflow-auto-paths.toml` at main's tip. It counts workflow
reviews only for a PR based on `main`.

`docs/roles/risk-paths.toml` holds these tables, each `paths = [ ... ]`:

| Table | Meaning |
| --- | --- |
| `operator` | Paths that always need the owner's approval. |
| `browser_qa` | A change to any of these paths needs a Browser QA pass. |
| `routine` | Ordinary documentation that may merge with no mandatory review (regular files only, never a sensitive path). |
| `schema`, `trigger1`, `trigger3`, `trigger4`, `trigger6`, `trigger7` | Sensitive areas: schema, trust and identity, secrets, supply chain and CI, outward actions, the rules themselves. A change here is `strict`: two reviews unless every changed path is on the one-review include list and off its exclude list. List these paths in `one_review_exclude` to always keep two. |
| `delegable` | The only paths a delegated approval can cover (see [What the human approval does](#what-the-human-approval-does)). |

`operator`, `schema`, `trigger4`, `trigger6` and `trigger7` paths are
human-class: they need the owner's approval (of the exact head, or a scope
approval that declares them; see below). A `trigger1` or `trigger3` path, or a
path the one-review list excludes, is `strict` without that floor: it needs the
owner's approval only for another reason, for example a verdict whose Risk is not
`auto`, more than 1500 changed lines, or the workflow rule below.

`docs/roles/workflow-auto-paths.toml` has an `[auto]` table with `paths`. When a
review workflow's result is counted, a path outside `[auto] paths` forces the
owner's approval, and an unreadable file fails closed.

`docs/roles/one-review-paths.toml` has `one_review_include` and
`one_review_exclude`: the paths where one combined review is enough. A path must
match an include and no exclude (exclude wins), and every change must be a plain
add or modify of a regular file. A delete, rename, type change or mode change
keeps two reviews.

Globs are repository-relative. `*` stays inside one path segment, `**` crosses
directories. Each table is required: a missing table, an invalid glob, or a
repeated table or key makes the whole file unusable.

Cadence evaluates a PR into one of three classes:

| Class | Reviews | Owner approval | Browser QA |
| --- | --- | --- | --- |
| `routine` | None mandatory; the required checks and ticket outcome evidence still apply (hosted: both review workflows; see below). | No | No |
| `consequential` | One combined review (standards and spec/security together). | No | If a path matches `browser_qa`. |
| `strict` | Two reviews (standards, spec/security), or one when every path is on the one-review list. | Yes, for `operator`, `schema`, `trigger4`, `trigger6` or `trigger7` paths, over 1500 changed lines, or when the lists are unusable. | If a path matches `browser_qa`. |

Anything the evaluator cannot place safely lands on `strict`: a custom or
unapproved `delivery:` section, a malformed section, unreadable or unusable
lists, incomplete diff evidence, a sensitive path, or a delete, rename or mode
change.

The strict floor when the lists are unusable is two reviews and the owner's
approval, and no Browser QA (the owner's approval is the human check there). A
repository that declares neither file gets the same floor today. The evaluator
also has a lighter default for a repository proven to declare none (one combined
review and the owner's approval, no path rules, no Browser QA), but neither the
hosted gate nor the native evaluator applies it yet.

Ask the board what a PR needs for its exact head:

```sh
cadence --org acme delivery requirements --issue <issue-id> \
  --pr https://github.com/<owner>/<repo>/pull/<number> --head <full-sha>
```

The answer carries `class`, `reviews`, `review_kind`, `operator_approval`,
`browser_qa`, `full_checks`, the `policy_digest` it was computed under and
`reasons`. Evidence binds to that digest, so a policy change invalidates it. The
caller needs `gh` read access to the repository.

The merge gate also counts the `standards` and `spec-security` review axes
itself, either as two review workflow results or as attested verdicts, and it
adds the owner's approval when a verdict states a Risk other than `auto`. The
requirements report is the policy's answer; always read the gate's own result
with `delivery check`.

## Review evidence

A review is by a session that is not the author and carries no conversation or
context from the work. A fresh subagent qualifies; a resumed or forked authoring
session does not. Record who reviewed, the repository, the full head, the outcome,
the checks run and the limits.

On a hosted org, evidence is project-scoped and pinned to the exact head. A new
commit voids it.

### Standards and spec/security

A hosted verdict counts only for a reviewer the workspace owner has attested.
The gate counts verdicts by attested principal and refuses the author's
principal. A review workflow's result counts when its principal is listed in the
repository's `docs/roles/review-workflow-principals.toml`, read at the anchor.

```sh
cadence --org acme audit verdict --project <project-key> --repo <owner>/<repo> \
  --issue <issue-id> --pr <number> --head <full-sha> \
  --kind standards --outcome pass --risk auto --file findings.md
```

- `--project` is required and never inferred from `--repo` or the working
  directory.
- `--kind` is `standards`, `spec-security`, `combined` or `browser-qa`.
  `--outcome` is `pass` or `revise`.
- The reviewer is the authenticated caller, never an argument. A repeat of the
  same kind, head and findings returns the latest row with `duplicate: true`. A
  changed outcome appends a correction, and the gate reads the latest row per
  reviewer and kind.
- Read the evidence back with `cadence --org acme audit verdicts --project
  <project-key> --repo <owner>/<repo> --issue <issue-id> --pr <number> --head
  <full-sha>`. Add `--body` to print each note's findings, which is how a
  `revise` is read. The text is the reviewer's, bounded, and control and invisible
  characters print as U+FFFD. It never decides a count.

The owner binds each principal's GitHub login once, then admits principals to a
project's review set. Both are the owner's own act and need a login made with
`--audit-approve`; an agent's call is refused.

```sh
cadence --org acme audit identities set \
  --identity <subject-id>=<github-login> [--identity ...]
cadence --org acme audit reviewers set --project <project-key> \
  --reviewer <subject-id> [--reviewer ...]
cadence --org acme audit reviewers --project <project-key>   # read both joins back
cadence --org acme audit identities                           # the workspace map
```

Both `set` verbs replace the whole map, so repeat the flag for every entry you
want to keep. A filed row counts only while its principal is in the project's
current reviewer set and has a matching attested login. Leaving a principal out
stops new verdicts and stops its earlier rows counting. A principal with no
attested login reads back with `github_login: null`, and its verdicts cannot
count. Never impersonate a
reviewer: the verdict names the authenticated caller.

### Browser QA

Required when the requirements report `browser_qa: true`. A fresh QA session
exercises the actual change on a real route and reports the result and limits.
Browser QA is met by any one of these routes:

1. **A counted `browser-qa` verdict**: `audit verdict --kind browser-qa
   --outcome pass`, from an attested, eligible reviewer.
2. **A non-author member's pass comment.** A repository member who is not the PR
   author posts a separate, unedited comment with exactly these two lines and the
   full current head:

   ```text
   Browser QA: pass
   Head: <full-sha>
   ```

   The full report goes in another comment.
3. **The author's own pass, accepted by the owner.** The PR author posts the same
   exact two-line comment, and the owner's per-head approval of that same head
   accepts it. The owner reads the report first.

A scope approval or a delegated approval never accepts Browser QA. An edited
comment or one for an older head does not count; new commits need new evidence.
The gate checks the pass and the approval, not whether QA ran, so the owner's
approval accepts that. `delivery check` reports `browser_qa_route`: an author pass
awaiting approval is not a pass by itself. Pass `--browser-qa` to `delivery check`
and `delivery enqueue` to require it:

```sh
cadence --org acme delivery check --project <project-key> --issue <issue-id> \
  --repo <owner>/<repo> --pr <number> --head <full-sha> --browser-qa
```

## What the human approval does

The owner's approval is a record that a human read the evidence and accepts that
exact head. It is the one thing an agent cannot supply.

- It names one head. A later head needs a new approval.
- It is project-scoped and tied to one issue; an approval for another ticket
  never answers this one.
- It is a claim with a `--source` (who approved, where). It is never `user` or
  `daemon`.
- It grants only the action it names, a merge. It does not accept a failed
  review, and it never substitutes for a missing required check.
- A scope approval pre-approves a ticket's scope. It refuses `trigger1` and
  `trigger3` paths, paths the one-review list excludes, `operator` paths that are
  in no trigger table, and triggers the ticket does not declare. It covers
  `schema`, `trigger4`, `trigger6` and `trigger7` paths that the ticket's
  `Risk: human (...)` line declares.
- A delegated approval (recorded by a designated agent) covers only paths in the
  `[delegable]` table of `risk-paths.toml`, and a `schema` path only under a live
  scope approval. Both kinds read that table, so a file without it is unusable
  for them.
- Neither kind accepts Browser QA.

When a review workflow says `revise` and the owner judges it wrong, the owner
reads the findings and settles it for that head only:

```sh
cadence --org acme audit findings --project <project-key> --issue <issue-id> \
  --repo <owner>/<repo> --pr <number> --head <full-sha>
cadence --org acme audit accept-revise --project <project-key> --issue <issue-id> \
  --repo <owner>/<repo> --pr <number> --head <full-sha> --source "<who, where>"
```

This lifts that one refusal. An owner's own `revise`, a missing check, a scope or
a missing merge approval still refuse.

## Hosted operator commands

These are the owner's, run in a plain terminal. An agent never runs them: the
board refuses an agent connection. Agents may run the read-only checks.

```sh
# 1. Read the gate (read-only; exit 0 only when every rule passes).
cadence --org acme delivery check --project <project-key> --issue <issue-id> \
  --repo <owner>/<repo> --pr <number> --head <full-sha>

# 2. Record the approval for that exact head.
cadence --org acme audit approve --project <project-key> --issue <issue-id> \
  --repo <owner>/<repo> --pr <number> --head <full-sha> --source "<who, where>"

# 3. Evaluate the gate, record the evidence, sign it and enable auto-merge.
cadence --org acme delivery enqueue --project <project-key> --issue <issue-id> \
  --repo <owner>/<repo> --pr <number> --head <full-sha>
```

Newer clients can record the approval and enqueue in one call with
`delivery enqueue --approve "<source>"`. Nothing is recorded when any other gate
rule fails, and the receipt says whether the approval was recorded. A board
that predates the option reports the outcome as unknown: read the approval back
with `audit approval` before repeating.

Related owner verbs: `audit revoke <id> --reason <why>` withdraws an approval
(and dequeues a PR that is merging), `audit approval` reads whether a merge
approval is in force for a head, and `audit designate` / `designations` / `digest`
manage delegated approvals.

## Refusals

`delivery check` exits 0 with `state: pass` and exits 1 with `state: refused` and
a `reasons` list. `delivery enqueue` answers `state: enqueued` or refuses. Read the
`reasons`; each names the rule that failed.

| You see | Meaning and what to do |
| --- | --- |
| PR is not open; the title's first issue token does not match the issue; the requested head is not the current head | The requirements are for one open PR at one head. Fix the PR or pass the current full head. |
| PR base is neither the default branch nor an approved `integration_branch` | Target the default branch, or have the owner approve the project's integration branch. |
| The comparison file list is truncated (300 files or more) or does not match the trees | The change is too large to classify. Split it. |
| The PR head, base or policy changed during collection | Run the command again. |
| `repo_not_bound` | The project has not declared that repository. The owner records it with `issue project delivery`. |
| `project_mismatch` | The issue belongs to another project. |
| `reviewer_not_bound` | The caller is not made eligible for the project. The owner runs `audit reviewers set`. |
| `identity_unattested` | The caller has no attested GitHub login. The owner runs `audit identities set`. |
| `409 container_required`, or a read-side `not_implemented`, `invalid_request` or 503 `capability_unavailable` on a gate verb | The board cannot serve the verb: the tenant server is older than this client. Update the tenant server first, then the client. The gate treats this as a refusal, never as "no evidence was filed", and the client never falls back to older repository-scoped evidence. |
| A write-side 503 `capability_unavailable` | The outcome is unknown. Read the evidence back (`audit approval`, `audit verdicts`) before retrying. |
| `gate_refused`, `head_moved`, `not_permitted` | The gate refused a rule (read its `reasons`), the PR head moved since you read it (check again at the new head), or the caller may not do this (owner verbs are owner-only). |
| An agent's call to an owner verb is refused | By design: approval, enqueue, `accept-revise`, identities and reviewers are owner-only. |

A missing hosted read is never a pass. If the board cannot answer, the gate
refuses.

## Local and hosted

| | Native install | Hosted workspace |
| --- | --- | --- |
| Gate | The local daemon computes the requirements (`delivery requirements`) and lands a PASSed ticket (`delivery merge`). | The board runs the gate; `delivery check` and `delivery enqueue` are hosted-only and refused on a native board. |
| Approval | Recorded by the operator's own connection. | Recorded by the workspace owner with the `audit.approve` login. |
| Review evidence | Review notes in a notes directory, counted by their declared reviewer. | Verdicts counted by attested principal; the author's principal is refused. |
| Delivery policy | `PROJECT.md` plus `issue project approve-work`. | `issue project delivery` by the owner. |
| Lane release | `issue finish`. | `issue finish` with a narrower set of options; see [Hosted workspace](hosted-workspace.md). |
| Exception routes | The native gate has a local exception route that waives missing reviewer attestation until `EXCEPTION_ENDS`; it cannot land a PR while local signing is retired. | None. Hosted refuses it. |

A hosted `routine` PR has no readiness record, so it takes both review workflow
passes and any approval the gate requires. Whenever a workflow review is counted,
`docs/roles/workflow-auto-paths.toml` decides: a path outside `[auto] paths`
needs the owner, and so do protected paths, case variants of them, and anything
other than a plain add or modify. An unreadable file fails closed.

## Document review for non-code work

The lifecycle is the same. A document replaces the pull request:

1. The creator saves the file with `artifact put --label <type>` and submits it
   with `artifact submit <art-id>`.
2. The type's approver approves that exact version on the board's Documents
   screen. There is no approve command. A member cannot approve a version they
   wrote, and a later version needs its own approval.
3. The owner shares the approved version, which delivers it. Where a type needs
   approval, sharing any other version is refused (`document_not_approved`).

Document review is hosted only. See "Submitting a document for review" in
[Hosted workspace](hosted-workspace.md#submitting-a-document-for-review) for the
commands and their refusals, and [BOARD.md](../BOARD.md) for the Documents screen.
