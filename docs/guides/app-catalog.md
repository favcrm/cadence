# Workspace app catalog

`cadence app catalog` installs and inspects validated workflow bundles without
requiring a project. The default workspace is the daemon's PM root. This catalog
increment provides installation and discovery; it does not run an app, approve
it, create worker grants, or load custom app UI.

All catalog commands, including reads, require a reachable daemon and an operator
caller. They fail when the daemon is unavailable; there is no offline catalog
fallback. Agent read permissions and Local/AgenticOS execution are future
CAD-631/CAD-632 work. Custom app UI remains separate CAD-633 work.

## Commands

| Command | Behavior |
| --- | --- |
| `cadence app catalog install <source>` | Validate and copy a local bundle or Git repository root into the workspace; return a new immutable installation ID. |
| `cadence app catalog ls` | Return an array of catalogued installation descriptions. Never initializes or migrates the catalog. |
| `cadence app catalog show <install-id>` | Inspect one exact installation ID. An app name is not a substitute. |
| `cadence app catalog migrate` | Explicitly catalogue legacy installations and backfill missing IDs, using the PM mutation and Git delivery path. |
| `cadence app catalog recover <install-id>` | Resume delivery from that installation's retained install journal. |

These commands do not accept `--project`. New workspace installations have
`project: null` and `project_link: null`; no project is created. Catalogued legacy
installations retain their project metadata. A project link does not grant
authority.

## Install a local bundle

The following examples assume an operator is using an intended development or
test workspace and the current directory is a trusted Cadence checkout. They
describe commands, not a production rollout.

```sh
cadence app catalog install ./apps/blog-post
cadence app catalog install ./apps/social-content
cadence app catalog ls
```

The CLI resolves a local source relative to the caller's current directory and
sends its canonical absolute path to the daemon. The daemon validates that source
again; it does not interpret the path relative to its own working directory.
Sources inside the PM tracker are rejected.

Copy the returned `install_id` when inspecting an installation:

```sh
cadence app catalog show <install-id>
```

The source must be an A1 bundle: `app.md`, checked workflow Markdown under
`workflows/`, and supported optional `rubrics/` and `templates/` files. See the
existing [Blog post bundle](../../apps/blog-post/app.md) and
[Social Content bundle](../../apps/social-content/app.md).

The React fixture preview at `app-previews/social-content` is **not** an
installation bundle. Its independent development harness is documented in
[app-previews/README.md](../../app-previews/README.md). Installing the workflow
bundle does not connect or deploy that preview.

OpenSlide's upstream repository is not a Cadence bundle. It needs the future
Cadence wrapper tracked by CAD-624; its raw repository URL is not a supported
app installation source today.

## Sources and commit receipts

A Git source must have the bundle at its repository root. The current resolver
clones default HEAD and records the actual resolved Git commit SHA in `source`.
That receipt identifies the content copied during this installation; it does
not provide selectable `--revision`, URL@SHA, branch, or repository-subdirectory
support.

For a particular commit of a repository containing nested bundles, use a trusted
local checkout already detached at the verified commit and pass the bundle's
folder. The receipt remains local-path provenance; it does not claim the catalog
selected or verified that Git revision. A Cadence repository root URL is not a
shortcut for its nested `apps/blog-post` or `apps/social-content` folder.

## IDs, repeats, and result fields

Use `install_id` as the installation identity. Same-name legacy installations in
different projects remain distinct. Migration preserves existing nonempty IDs
and backfills only missing IDs; it does not rewrite teams or legacy approval
history.

Only one workspace installation of a given app name is supported in this
increment. Repeating an install fails with an already-installed error naming
the existing ID. It does not allocate a duplicate, replace the bundle, or widen
approval. Workspace replacement, upgrade, removal, approval, and execution are
not catalog commands in this increment. Do not use a workspace ID with legacy
name-based update or approval commands.

Install and show return a description object; ls returns an array of such
objects. Useful fields include:

| Field | Meaning |
| --- | --- |
| `schema`, `workspace`, `catalog_generation` | Result schema, default workspace, and the published catalog generation inspected. |
| `install_id`, `name`, `title`, `version`, `summary` | Stable identity and manifest metadata. |
| `project`, `project_link`, `storage_kind` | Optional legacy project metadata and `workspace` or `legacy` storage. |
| `source`, `digest`, `installed_at` | Recorded source provenance, copied content digest, and installation time. |
| `approval.state`, `approved`, `executable` | New workspace rows are `unapproved`, `false`, and `false`. Legacy catalog rows report approval `unknown` and `approved: null`; consult the existing legacy surface for approval verification. |
| `guide`, `record`, `files` | Manifest guide, retained installation record, and bundle inventory. |

Successful install/recover responses also report `committed: true` and
`foreign_files` from PM delivery. Install adds validation `notes` and
`secret_warnings`. Migration instead returns a summary with the generation,
installation count, delivery fields, and `executable: false`.

`executable: false` describes this catalog surface. It does not disable or
replace already-established legacy execution paths. Adding an installation
creates no bindings, team assignments, worker grants, or authority.

## Legacy compatibility and migration

Existing commands retain their meaning:

```sh
cadence app ls
cadence app ls --project site
cadence app show social-content --project site
```

Legacy `app ls` without a project still lists every project's apps. Exact
name-plus-project lookup remains available, including before catalog migration.
Legacy reads retain their existing offline behavior; catalog commands do not
inherit it. The legacy Apps board is a separate surface.

A missing catalog causes catalog ls/show to fail rather than silently migrate.
If legacy installations exist, workspace installation also requires explicit
migration first:

```sh
cadence app catalog migrate
cadence app catalog ls
```

In a workspace without legacy installations, the first workspace install can
create the catalog. Migration uses retained journals and preserves workspace
entries as well as legacy identities. Repeating migration must not create new
identities for existing installations.

## Interrupted delivery

Installation retains a journal under `.apps/install-journals/`. If copying,
publication, or PM Git delivery fails, pending state makes catalog reads refuse
instead of presenting an installation as delivered. Retain the reported
installation ID and, after resolving the underlying delivery problem, resume:

```sh
cadence app catalog recover <install-id>
```

Recovery validates the retained journal, current catalog, and staged contents;
it refuses conflicting or changed state. It resumes the same installation and
PM commit, not an upgrade or an automatic backup restore. It has no rollback
flag. A missing journal cannot be recovered with an arbitrary app name.

This public recover command handles **installation journals only**. Migration
journals use a different format; interrupted migration recovery/rollback is not
exposed by this CLI command. Do not pass a migration journal ID to it or edit
catalog files to bypass a pending-publication refusal. Preserve the error and
journal details for a separately scoped recovery investigation.

## Explicit catalog migration recovery

`cadence app catalog migration-recover <journal-id>` explicitly resumes a retained migration journal; add `--rollback` only when choosing restoration of its verified preimages. This is separate from `recover <install-id>`, which retries installation delivery. Both require operator authority and the daemon, retain divergence refusal, and use normal scoped PM Git delivery. HTTP equivalents are `POST /api/app-installations/migrate` with `{}`, `POST /api/app-installations/<install-id>/recover` with `{}`, and `POST /api/app-installations/migrations/<journal-id>/recover` with `{"rollback": false}` (or explicit `true`). No endpoint grants approval or execution authority.
