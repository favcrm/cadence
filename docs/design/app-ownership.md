# App ownership from the actual use cases

Date: 2026-09-26. Issue: CAD-629. Design and delivery plan; no runtime change in this PR.

The operator approved correcting the project-owned app model. The current
implementation remains project-scoped until the slices below are delivered.
This document supersedes the mandatory-project assumption in the earlier
Apps A1 and Social Content notes, not their unrelated workflow or UI decisions.

## Product contract

An app installation belongs to a workspace. An app owns its runs and records.
A project is an optional link for organizing related work, not a prerequisite
for installing or using an app. A repository is a possible destination and
working resource, not an app's owner.

For the initial single-workspace deployment, explicitly identify the existing
PM root as the default workspace. Do not create a hidden project or infer
workspace ownership from the currently selected project. Multiple-workspace
administration and authentication are separate future work; this change must
not imply those capabilities already exist.

## Scenarios the model must serve

| Scenario | Ownership and context | Project and destination |
|---|---|---|
| Owner drafts one social post | Workspace Social Content installation; default brand or explicit inputs | No project; local outbox |
| Agency manages two clients | One installation, separate client contexts, separately authorized sources, assets and accounts | Optional client campaigns; no cross-client data or grants |
| Marketing campaign uses social, blog and slides | Each app owns its runs; project links related runs | One campaign project spans apps; each output has its own destination |
| Blog published from a repository | Workspace Blog Post installation; publication settings | Repository explicitly selected and authorized as a destination/resource |
| One-off Open Slide deck | Workspace Open Slide installation; supplied audience and material | No project or client required; deck export |
| Same app already installed in two projects | Two preserved installation identities during migration | No automatic merge of teams, records or approvals |
| Project archived or deleted | App, contexts, records and history remain owned by workspace | Link can be removed; app is not deleted |
| Client removed or connection revoked | Access stops; history retained according to explicit retention policy | Pending effects cannot use stale grants |

## Relationships and user flow

- Workspace contains app installations and projects.
- App installation identifies its package, configuration, team defaults,
  approved capabilities and owned records/runs.
- App context optionally isolates a brand/client's configuration, assets,
  sources, destinations and work. It is not a global CRM client registry.
- Run pins installation identity, context when applicable, workflow revision,
  inputs, actual team and the authorization-relevant configuration revision.
  Project link is optional. Changing a UI selection cannot change a live run.
- Outputs belong to their run; a destination receives them through an
  authorized effect. Project links provide discovery, not permission.

Apps → Social Content → choose a brand when necessary → select sources or
start New post → inspect drafts → review/release → inspect outcomes.
For a one-off context-free app, open it and supply the inputs directly.
Project pages may show linked runs across apps; they are not workflow owners.
`site` remains a possible Blog Post repository destination and legacy project
link, not the mandatory owner of Social Content.

App team defaults are a convenience. Context-specific assignments and per-run
overrides must be explicit and checked; a shared reviewer cannot silently
inherit access to every client. Reviewer independence remains mandatory where
the workflow declares it. No worker auto-enrollment or implicit privileges.

## Authorization invariants

Workspace ownership does not grant all workspace agents access to all app
contexts. The daemon checks the caller, installation, context, resource,
action and current grant. The board/HTTP path must enforce the same proof.
Execution calls must also prove the run assignment: a shared agent's union of
account scopes is not sufficient. Bind the capability envelope to the run,
installation and context, and validate it at the effect/output broker.

Approve configuration and capabilities against installation identity and the
authorization-relevant digest, including context connection bindings. Changing
a destination or context's connection requires renewed approval for affected
capabilities. A team change derives only the approved scopes for newly assigned
members and removes stale member grants; assignment itself cannot widen scopes.

Outbound effects carry installation/context/resource attribution and remain
subject to the existing operator release policy. Revocation, deletion or a
changed authorization revision invalidates pending effects as appropriate.
Project linking never confers connection access or app-edit authority. Preserve
the app-team self-edit restriction work (CAD-622), independent of project links.

Keep three approval layers distinct: installation approval authorizes the
configured capability boundary; run/step execution requires the workflow's
declared execution approval or explicit scoped standing approval; outbound
effects require their release approval. Package approval alone cannot dispatch
arbitrary workflows, and project-free runs retain execution gates.

Scope record reads, assets, runs, outputs, search and event subscriptions as
well as writes. Reject forged workspace/context fields, guessed IDs and stale
references. Do not use a context dropdown as an authorization mechanism.
Preview/download, effect review and resume require the same scope, including
local artifacts. Context IDs are namespaced by workspace and installation.
Persist immutable provenance on runs and effects rather than inferring it from
the selected project or an app/workflow string. Context deletion archives its
history and revokes new execution/effects; it never falls back to workspace
default connections. Resume cannot silently substitute a new team or account;
material changes require an explicit authorized revision or a new run.

## Existing coupling verified in code

- `src/issue/app.rs`: install validates a registered project; folders and
  records live under `<pm>/<project>/apps`; approval keys are `project/app`.
- `src/ui/apps.rs`: detail, run, output and approve routes use project/name.
- `src/issue/plan.rs`: plan creation and discovery use project ticket folders.
- The current installed `site/blog-post` record is unapproved and has an empty
  default team. Social Content is not installed in `site` as of this inspection.
- Social Content prototype #307 already contains a client switcher, but is a
  draft mock without daemon wiring. Keep it as the visual reference.

Consequently removing a project chip from the UI is insufficient. Run storage,
dispatch and authorization must support absent project membership explicitly.
Existing generic jobs can inform execution, but do not assume they already
provide the app run lifecycle, record storage or approval semantics.

## Compatibility and migration

Give every installation a stable identity; never authorize by app name alone.
Preserve each legacy installation independently, even when package names match.
Preserve its original project link, content, history, source/version and team
information. Legacy callers may resolve project/name to that exact installation
through an adapter; do not reinterpret an ambiguous name as a global lookup.

Moving storage must not widen old grants. Keep legacy scope restrictions until
explicit operator reapproval derives the new scoped grants. Do not silently
translate old project-wide permissions into workspace-wide permissions. A staged
migration may leave moved installations unapproved with an explicit Needs-you
item while retaining readable history and preventing new dispatch/effects.

Migration must be repeatable, backed up and recoverable; enumerate storage and
database version changes before implementation. Preserve pending work and its
original authorization boundary. Do not re-dispatch completed work or alter
queued effects during conversion. Duplicate names require visible labels and
an explicit later consolidation action, never automatic deduplication.

## Delivery slices

1. **Design and inventory (this ticket):** glossary, scenarios, concrete code
   coupling, authorization and migration contract, independent design review.
2. **Workspace installation and compatibility:** stable installation IDs,
   explicit default workspace, storage/API lookup, legacy adapter and migration
   with unchanged permissions. No fake project. Prove install/list/show without
   a project and preserve two legacy copies of the same app.
3. **App-owned execution:** run/input/team/output lifecycle with nullable project
   links, audited approvals and dispatch. Prove a no-project Open Slide or local
   content run can complete, not merely appear in the Apps list. Keep the
   project ticket adapter for existing runs until equivalent lifecycle evidence.
   Persist an app run as its own lifecycle object; an optional project plan is
   a linkage, not the only storage for steps, approvals or execution state.
4. **Optional contexts and scoped connections:** context-aware records, assets,
   team bindings, grants and effects; test two-client isolation and revocation
   before enabling real external connectors. Connections CAD-585 is coordinated
   work, not a prerequisite for a context-free local demo.
5. **Social Content preview and wiring:** stable private lane preview; first
   inspectable flow is app → brand/input → run → draft → review → local outbox.
   Use mock #307's visual direction; clearly label unfinished pieces. Then
   deliver library/editor and additional flows incrementally. Actual external
   publishing follows connection readiness and explicit approval.

Each runtime slice is a separately reviewable ticket/PR with its own migration
and rollout evidence. All enforced gates get adversarial tests first: agent
caller, detached child, concurrent calls, forged fields, cross-context reads
and effects, and corresponding HTTP peer proofs. Never weaken a check.

For UI work follow the preview-led kickoff at
`/var/www/agent-notes/20260926-192226-f7249-preview-led-ui-development-kickoff.md`:
early stable private preview, revision/update time, usable increments, honest
mock boundaries, desktop/narrow state checks, milestone reports. Production
state, port 3010 and other lanes remain separate. PRs need independent head
review and pinned merge queue; rollout stays a distinct authorized operation.

## Acceptance examples for implementation tickets

- Install and run an app when no project exists; no hidden project created.
- A context-free local run still requires its declared execution approval;
  installation approval alone cannot dispatch it or release an outward effect.
- One campaign links runs from three apps without transferring ownership.
- Two clients cannot see each other's records, assets, events or effects.
- A forged context ID or project link never expands a caller's capabilities.
- Revoke a context connection while an effect waits; release cannot send it.
- Two legacy Social Content installations migrate independently and retain
  history; no old approval becomes wider and no queued work executes twice.
- Archive a project; app and run history remain available to authorized users.
- Publish one stable private Social Content preview before production rollout.

## Non-goals

This design does not introduce a client CRM, multi-tenant hosting, automatic
credential transfer, automatic app approval, unrestricted app self-editing or
real Instagram/Facebook publishing. Open Slide remains CAD-624 until its app
implementation lane begins. CAD-629 changes the ownership contract; it does not
claim the current production app engine already satisfies that contract.
