# Workspace app catalog

`cadence app catalog` installs and inspects validated workflow bundles without
requiring a project. The default workspace is the daemon's PM root. An operator's
install or update records consent for the installed digest when its local
execution contract passes validation. Install and update do not create worker
grants; the bundle's declared `app-screens` package is the app UI. Run
approvals and outward-effect permissions remain separate.

For a new app, start with [App development](app-development.md); this guide
remains the catalog and install contract.

Local catalog commands, including reads, require a reachable daemon and an operator
caller. They fail when the daemon is unavailable; there is no offline catalog
fallback. The selected-remote published-release journey below instead uses the
saved renewable human login and live workspace owner authority.

For the overall local/hosted setup, agent registration and notification boundaries,
see [Hosted workspace](hosted-workspace.md). This guide retains the exact package
and published-release command contract; installing a package does not connect an
agent or migrate native business storage.

## Commands

| Command | Behavior |
| --- | --- |
| `cadence app catalog install-check <source>` | Read-only. Resolve and validate the bundle exactly as install would; return `schema`, `name`, `version`, `source`, `digest`, the `files` list, `committed` (always `false`), `readiness` (`{"ready": true, "reason": null}` — a fresh install has no active work), `access_diff`, `notes` and `secret_warnings`. Writes no catalog, journal or record. |
| `cadence app catalog approve <install-id> --digest <digest>` | Operator-only. Approve the exact current installed digest for supported local artifact steps, after validating its local execution contract. Refuses a stale digest. |
| `cadence app catalog revoke <install-id> --digest <digest>` | Operator-only. The digest pins the request to the current installed bundle; stale digests are refused. Revoke removes local capability authority installation-wide, revoking historical epochs and invalidating effects for every version, including completed historical work. Does not change legacy project approvals. |
| `cadence app catalog install <source> [--expected-digest sha256:…]` | Validate and copy a local bundle or Git repository root into the workspace; return a new immutable installation ID. With `--expected-digest` the install is refused, before any write, unless the resolved bytes hash to that digest. |
| `cadence app catalog ls` | Return an array of catalogued installation descriptions. Never initializes or migrates the catalog. |
| `cadence app catalog show <install-id>` | Inspect one exact installation ID. An app name is not a substitute. |
| `cadence app catalog migrate` | Explicitly catalogue legacy installations and backfill missing IDs, using the PM mutation and Git delivery path. |
| `cadence app catalog recover <install-id>` | Resume delivery from that installation's retained install journal. |
| `cadence app catalog migration-recover <journal-id> [--rollback]` | Explicitly resume migration delivery, or roll it back when `--rollback` is supplied. |

These commands do not accept `--project`. New workspace installations have
`project: null` and `project_link: null`; no project is created. Catalogued legacy
installations retain their project metadata. A project link does not grant
authority. Install and update are operator-only and record consent for the exact
installed digest if it passes the local execution checks; `approve` and `revoke`
remain available for explicit operator decisions about local capabilities.

## Selected remote: immutable published releases

With `--org <workspace>`, local paths and Git-source catalog install/update commands
are refused: they never upload files or fall back to this machine's local install.
The remote journey names an exact published listing/version/digest. Obtain these
from the catalogue's `/public/apps/<listing>/versions` metadata, then:

```sh
cadence --org <workspace> app catalog release-check <listing> --version <version> --digest <sha256-digest> > review.json
# Inspect the full review.json, including access, compatibility, warnings and readiness.
cadence --org <workspace> app catalog release-install <listing> --version <version> --digest <sha256-digest> --operation-id <uuid> --review review.json
cadence --org <workspace> app catalog ls
cadence --org <workspace> app catalog show <install-id>
```

The `release-check` output is JSON. Preserve its full successful response in the
review file; apply rechecks and compares the exact structured review. Consent
never follows changed bytes, access, readiness, or installation generation.
For update, check with `--install-id <id> --expected-digest <old-digest>
--expected-generation <generation>`, then use `release-update <id> <listing>`
with those old pins plus `--version`, `--digest`, `--operation-id` and `--review`.
Use the `generation` returned by the hosted installation row, not a guessed value.

`release-revoke <id> --digest <current-digest> --expected-generation <generation>
--operation-id <uuid>` withdraws package approval using the native owner's revoke
semantics; it does not delete business data or settle uncertain external effects.
These hosted commands require a plain human shell with a saved login, not a
managed agent spending its owner's credential. No browser cookies are copied.

Retain the operation UUID printed on stderr, including after an error:

```sh
cadence --org <workspace> app catalog operation <uuid>
cadence --org <workspace> app catalog pending <app-name>
cadence --org <workspace> app catalog reconcile <uuid>
```

A current owner can discover/reconcile a pending operation even after its creator
leaves. Recovery observes the native catalog or cancels an undispatched preparation;
it never repeats a mutation. `prepared`/`dispatched` do not mean executed. Exit 5
means unresolved; exit 3 means refusal/unavailable. A lost response does not
authorize retrying with a new UUID. The same operation binds the same actor,
release, old installation pins, and checked review; changed input conflicts.
Installation grants no send/deploy, worker registration, or run approval.
Native install/run/business records remain native-owned; this is not their migration.

## Offline byte validation: `cadence app check`

`cadence app check <dir>` validates package bytes with no
daemon, no store and no PM read or write — usable in an app repo's CI before
anything talks to a host. The `builtin:<catalog-id>` source form is retired
(CAD-1422) and refused at the source stage (`app_check_source`). It runs the same bundle snapshot and content
validation `install-check` runs (entry grammar, per-file caps — 384 KiB for a
screen JS/CSS asset and 256 KiB elsewhere — the 2 MiB aggregate, the manifest
including `summary`, descriptor forbidden keys, workflow checks, the full
`app-screens/v1|v2` declaration integrity for every `screens/<tag>/` package
(`screens.json` body, declared↔supplied asset set, sha256+size matches — the
same `app_screen_pkg::extract` the mount RPC re-runs), `requires`
against this build's contract registry, and the secret guard) and reports the
same `sha256:` bundle digest. A git/URL source is refused: fetching is a
transport the daemon vets, not bytes in hand.

The version-2 bundle member grammar also recognizes the root
`app-storage.json` and flat `storage/<tag>.json` migration members. The tag uses
the same lowercase ASCII/digit/hyphen grammar as other app tags (1–32 bytes,
no leading hyphen); links, nested storage directories and alternate extensions
are refused. Storage members require exactly
`needs.requires.contracts: {app-storage: [1]}`, and that requirement needs its
root declaration. The generated document is
`contracts/bundle-grammar/v2/grammar.json`; version 1 remains a historical
artifact, not a second current admission path.

This is package grammar support, **not a native storage runtime**. The native
host does not advertise `app-storage`, execute these migrations or claim schema
validation. `cadence app check` inventories the new members and returns the
explicit incompatible-host receipt for a storage-bearing package. A hosted
implementation must separately validate the declaration and checksums, enforce
its tenant-local storage contract and bind activation to the same immutable
bundle. An unknown requirement is never treated as supported.
Explicit approval of edited installed bytes rechecks the same requirement and
storage-member pairing before recording execution authority. Unsupported bytes
remain inspectable; an operator approval cannot supply a missing runtime.


The JSON report carries `ok`, a stable `refusal.code` (`app_check_transport`,
`app_check_source`, `app_check_inventory`, `app_check_content`), `digest`,
per-file `bytes`/`cap`, the 2 MiB aggregate position, and the `host` block and
`compatibility` receipt (this build's core version and supported contract
majors). Exit is 0 on pass, 3 on refusal. Pass and refusal write nothing — no
state directory, catalog, journal or tracker file is touched.

Byte validation only — stateful admission is deliberately not reproduced:
`install-check` also enforces the catalog (a same-name workspace install,
pending journals, an unmigrated legacy install), the tracker-self-source guard
and the PM's `agent:` registry. Offline, the registry is empty
(`agent_registry: "none"`), so a concrete `agent:` name is the same
unverifiable note install-check reports on a host with no registry — never an
offline refusal. A bundle relying on that registry still meets it when the
daemon runs `install-check`/`install`.

## Pin the exact bytes you consented to

Install is consent, so consent must be to exact bytes. Check first, then pin:

```sh
cadence app catalog install-check ./tests/fixtures/apps/blog-post      # returns "digest"
cadence app catalog install ./tests/fixtures/apps/blog-post --expected-digest 'sha256:…'
```

The digest is the same bundle digest `upgrade-check`/`upgrade
--expected-new-digest` use, and the one `show` reports after install. If the
source changed after the check (including a moved Git HEAD) the install is
refused with the catalog, journal and records untouched. Omitting
`--expected-digest` keeps the unpinned behavior for local development. The board
relay `POST /api/app-installations` (and `/upload`) accepts the same optional
`expected_digest` field with the same refusal.

An optional `catalogue` object `{id, version, digest}` may travel with
`install`, `install-check`, `upgrade` and `upgrade-check` (CLI flags
`--catalogue-id`, `--catalogue-version` and `--catalogue-digest`, all three
together). It names a directory release. It is not a grant, and the daemon
stores it only when the package version and digest match the locator and the
closed directory `/public/apps/<id>/versions` metadata lists that exact release
as installable. A fresh install may use an older installable release. A
catalogue update refuses an older `MAJOR.MINOR.PATCH` version, and refuses
versions that cannot be ordered. Omitting `catalogue` does not record an
origin and does not strip one that is already stored. A path or Git update
that intentionally leaves the catalogue sends `catalogue_resolution: "manual"`
(`--catalogue-resolution manual`), which is mutually exclusive with the
locator and clears the stored origin. Unavailable or unreadable metadata does
not authorize the claim. A direct path or Git install without these fields is
unchanged and records no catalogue origin.

The board's install review (Explorer, app detail and "Add from Git URL") is
never unpinned: it calls `POST /api/app-installations/check` first, shows the
name, version, digest, file list, notes and secret warnings, and then installs
with the checked digest. The check route takes `source` plus the optional
catalogue locator or `catalogue_resolution` (a repeated or unknown key is
refused), is operator-only like install, and relays
`app_workspace_install_check`; it creates no catalog. A bundle that
changed after the check is refused and the review asks for a fresh check.

CAD-1422: the host's embedded built-in catalog is retired — `POST
/api/app-catalog/install` and `builtin:<id>` sources refuse. Published apps
are listed on the board at `/apps/explore` and `/apps/catalog/<id>`. Install
stays operator-only and digest-pinned. `GET /api/app-catalog*` still
answers for the catalog ids a historical `Source::Builtin` install or an
open install request names — a tombstone row with the retired reason and
the install's real state — and refuses anything else.

The directory is rendered by the Cadence UI, not by the catalogue API or a
separate server-rendered page. It is a signed-in surface: the board serves its
documents, assets and catalogue reads only to a live board session, and a
signed-out visitor is redirected to the platform sign-in (a public catalogue
belongs on a public origin, not on a tenant board). Its browser requests stay on
the board origin
(`/api/public-apps`, `/api/public-apps/<id>` and `/api/public-apps/<id>/versions`);
the board reads catalogue JSON. `/versions` accepts no query. A release
document is `/api/public-apps/<id>/release?version=<exact>&digest=<sha256:…>`
with both keys required, in either order, and no extra or duplicate keys.
Hosted boards use `http://api.internal` for listing, detail, and versions
metadata only — that host never serves the release document. The board
proxies the pinned release fetch to `http://catalogue-packages.internal`, a
closed outbound broker that answers exactly `GET /public/apps/<id>/release`
with `version` and `digest`. A self-hosted catalog origin (`https` or
loopback) still proxies the pinned release document itself. The directory
install pins that version and digest and uploads the same locator; readback
must show it. The locator is provenance after that match, not a publisher
grant.

Browse-only releases remain visible with their description and availability;
they never acquire an Install action merely because their listing is valid.
An invalid catalogue answer is an error, not an empty list. An unavailable
installed-app check is not proof that an app is missing, so installation stays
off until the check succeeds. Anonymous visitors can browse and follow the
sign-in link; mutation authorization remains on the existing operator routes.

Legacy rule: while no workspace catalog exists, any unmigrated legacy
`<project>/apps/<name>` installation blocks every workspace install, not only
one of the same name, until `cadence app catalog migrate` has catalogued it.
After migration a legacy installation has its own ID and never blocks a
workspace install of the same name. The refusal names this rule and the
`migrate` command.

## Install a local bundle

The following examples assume an operator is using an intended development or
test workspace and the current directory is a trusted Cadence checkout. They
describe commands, not a production rollout.

```sh
cadence app catalog install ./tests/fixtures/apps/blog-post
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
`workflows/`, and supported optional `rubrics/`, `templates/` and `views/`
files. `views/` may carry exactly one file — `views/app-views-v1.json`, a
data-only [app-views/v1](../../contracts/app-views/v1/README.md) descriptor —
and only when `app.md` declares it with `needs.views.contract: app-views/v1`.
The declaration and the file pair up: either alone refuses install, and the
descriptor's `app` must be the bundle's own `app` name. Descriptor bytes are
inside the installed bundle's digest, so a byte change re-gates approval like
any other structural change; the verified `app catalog show` receipt serves
the validated descriptor as `view_descriptor` (plus `view_descriptor_digest`),
or `null` on bundles without one. Descriptors are declarations only — they do
not render live data or carry actions. See the existing
[Blog post bundle](../../tests/fixtures/apps/blog-post/app.md) and
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
shortcut for its nested `tests/fixtures/apps/blog-post` or `apps/social-content` folder.

## IDs, repeats, and result fields

Use `install_id` as the installation identity. Same-name legacy installations in
different projects remain distinct. Migration preserves existing nonempty IDs
and backfills only missing IDs; it does not rewrite teams or legacy approval
history.

Only one workspace installation of a given app name is supported in this
increment. Repeating an install fails with an already-installed error naming
the existing ID. It does not allocate a duplicate, replace the bundle, or widen
approval. Use the workspace `upgrade` commands to update it, and the `approve`
and `revoke` commands above to manage local capabilities. Do not use a workspace
ID with legacy name-based update or approval commands.

Install and show return a description object; ls returns an array of such
objects. Useful fields include:

| Field | Meaning |
| --- | --- |
| `schema`, `workspace`, `catalog_generation` | Result schema, default workspace, and the published catalog generation inspected. |
| `install_id`, `name`, `title`, `version`, `summary` | Stable identity and manifest metadata. |
| `project`, `project_link`, `storage_kind` | Optional legacy project metadata and `workspace` or `legacy` storage. |
| `source`, `digest`, `installed_at` | Recorded source provenance, copied content digest, and installation time. |
| `approval.state`, `approved`, `executable` | Coarse catalog projections: the daemon overlays an approved status and `approved: true`, `executable: true` only when live capability status for this installation ID and digest is approved; otherwise the catalog's base values remain. They are not an exact revoked-state receipt. For exact current state, the daemon reads capability status for the installation ID and digest. Legacy catalog rows report approval `unknown` and `approved: null`; consult the existing legacy surface for approval verification. |
| `guide`, `record`, `files` | Manifest guide, retained installation record, and bundle inventory. |

Successful install/recover responses also report `committed: true` and
`foreign_files` from PM delivery. Install adds validation `notes` and
`secret_warnings`. Migration instead returns a summary with the generation,
installation count, delivery fields, and `executable: false`.

An approved catalog row's `executable: true` means supported local text-workflow
execution is available through `app run`; it does not authorize outward effects.
Adding an installation creates no bindings, team assignments, worker grants, or
authority beyond consent for the validated local execution contract.

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
journals use a different format; use the separate `migration-recover <journal-id>` command, with explicit
`--rollback` only when choosing restoration of verified preimages. Do not pass a
migration journal ID to the installation `recover` command or edit catalog files
to bypass a pending-publication refusal. Retain the journal and error if
divergence prevents recovery.

## Explicit catalog migration recovery

`cadence app catalog migration-recover <journal-id>` explicitly resumes a retained migration journal; add `--rollback` only when choosing restoration of its verified preimages. This is separate from `recover <install-id>`, which retries installation delivery. Both require operator authority and the daemon, retain divergence refusal, and use normal scoped PM Git delivery. HTTP equivalents are `POST /api/app-installations/migrate` with `{}`, `POST /api/app-installations/<install-id>/recover` with `{}`, and `POST /api/app-installations/migrations/<journal-id>/recover` with `{"rollback": false}` (or explicit `true`). No endpoint grants approval or execution authority.

## The `listing:` block (CAD-1129)

An optional `listing:` mapping in `app.md` frontmatter is the app's display copy
in the Explorer. It is plain text, covered by the bundle digest, and can declare
no capability, slot or workflow verb. Keys: `tagline` (≤80 chars), `icon` (an
`assets/*.svg` path), `screenshots` (≤5 `{file, caption}`), `category` (one of
marketing, customers, operations, finance, content, other), `tags` (≤5),
`publisher`, `about`, `can` (what the app can do), `access_notes` (a sentence
per declared connection or capability slot), `changes`, `setup` and `data`
(`stores`: what the app keeps, up to five sentences; `personal`: true when that
includes personal data). Unknown keys, HTML, markdown links, control characters
and any `cost`, `price` or `pricing` key refuse the bundle.

The detail page's data-access rows come from the manifest, not from the copy
alone: "Its own data" shows when `data.stores` is non-empty or `data.personal`
is true, and carries the "Personal data" chip only for `personal`.

## Soft remove and restore

Removing an app from the Explorer (`POST /api/app-installations/<id>/remove`,
after `remove-preview`) is a journaled soft remove: the record is marked
removed, its consent is revoked, queued publishes are cancelled and runs are
refused. Nothing is deleted. The app leaves Open and the sidebar and is listed
under "Recently removed" for 30 days. Within that window
`POST /api/app-installations/<id>/restore` clears the mark and re-records
consent for the same bytes. After the window it refuses: the install can only
stay removed, and the page no longer offers Restore. Restore remains available
in **Recently removed** on the Apps page and in historical Explorer details.
Directory cards are whole links without nested actions. The installed
gallery's cards open the app itself (`/apps/<key>`); an operator's small
settings action on each card opens `/apps/manage/<id>`.

## Explorer routes

In the board, `/apps` is the installed-app home, including Favorites and project
apps. **Explore apps** opens the native directory at `/apps/explore`. Each
directory card opens `/apps/catalog/<id>` with its overview, features, releases
and workspace panel; installed-gallery cards open the app at `/apps/<key>`, with
a small operator settings action to `/apps/manage/<id>`.

A verified operator selects an immutable published release, chooses **Review
installation** or **Review update**, reviews the checked package and access
changes, then explicitly confirms Apply. Fresh installs may choose an older
eligible release. Updates may skip the recommendation for an eligible intermediate
version, but never downgrade or run automatically. Apply rechecks publication,
digest and the checked installation/catalogue generation. A withdrawn release
is refused without changing the installed copy. An uncertain result instead
shows **Not confirmed** and requires **Check installation** before another apply;
leaving or reloading the page does not retry it.

Git and local package sources remain on the existing operator-managed path,
including **More ▾ → Add from Git URL…** and `/apps/manage/<id>`. Historical
embedded-catalogue IDs remain retired tombstones, not a second install source.

A verified operator in a writable session can check and apply updates for any
installed app at `/apps/manage/<id>`. Enter a package source, choose **Check
update**, review the proposed version, digest, changed files and compatibility
notes, then choose **Apply checked update**. Git installs are prefilled with
their recorded Git URL, and other installs start empty so the operator can
enter an absolute path or Git URL — a historical built-in install updates from
its detached repository checkout. The
Social Content **Settings → App package** panel remains available as well.

The `cadence app catalog upgrade-check` and `cadence app catalog upgrade`
commands accept absolute paths and Git URLs; `builtin:<catalog_id>` is
retired (CAD-1422) and refused.
The existing board upgrade-check and upgrade routes use the same pins and
refuse stale proposals. The remote CLI (`cadence --org …`) supports the
[selected-remote published-release journey](#selected-remote-immutable-published-releases)
(`release-check`, `release-install`, `release-update`, `release-revoke`, `ls`,
`show`, `operation`, `pending`, `reconcile`). Local path and Git-source
install/update commands are refused with `--org`; hosted operators use the
Manage page for those and for the UI.

The built-in catalog is retired (CAD-1422): the binary embeds no app bundles
or catalog index, and canonical apps ship from their own repositories' release
artifacts through the AgenticOS marketplace. An Explorer
`update_available` flag is shown only to operators and means an update check for
that installation found a different bundle digest and cached `has_update: true`;
it is not a general notice that a build exists.

All routes below are served by the board and relayed to the daemon; reads and
favourites are open to a verified member as well as the operator, everything
else is the operator's. Agents are refused on every one.

| Route | Who | Purpose |
| --- | --- | --- |
| `GET /api/app-catalog`, `/api/app-catalog/<id>` | operator, member | Retired-catalog tombstones (CAD-1422) for the ids a historical `Source::Builtin` install or an open request still names, with the install's real state (`available`, `installed`, `off`; a soft-removed app stays `available` with `removed: true`, its `install_id` and `restorable`) and `retired`/`retired_reason`. Members get no `digest`, `request_count` or `update_available`. |
| `POST /api/app-catalog/install` | operator | Retired (CAD-1422): always refused — the built-in catalog is gone. |
| `POST /api/app-catalog/git-check` | operator | Check a public https Git repository: `{url, git_ref?, dir?}`. |
| `POST /api/app-catalog/request` | member | Ask the operator to install; resolves only a catalog id a historical install or open request still names. |
| `GET /api/app-home` | operator, member | One row per installation with its attention state. |
| `GET/POST /api/app-favorites`, `/opened`, `/default` | operator, member (`default`: operator) | Pinned apps per person. |
| `GET /api/app-requests`, `POST /api/app-requests/dismiss` | operator | Install requests. |
| `POST /api/app-installations/<id>/update-check`, `/remove-preview`, `/remove`, `/restore` | operator | Manage one installation. |

A Git source passed to `install`, `install-check`, `upgrade` or `upgrade-check`
(CLI and board alike) is vetted exactly like the Explorer's Git check: a public
`https://` repository on a registered-shaped DNS name that resolves only to
globally routable addresses. SSH, `git@`, IP-literal, localhost-style and
credential-bearing URLs are refused; use an absolute local checkout for those.

## Host requirements (`needs.requires`)

A package may declare the host it needs in `app.md` frontmatter:

```yaml
needs:
  requires:
    core: ">=0.1.0-beta.2 <2.0.0"
    contracts:
      app-views: [1]
      app-chat: [1]
```

`core` is a bounded AND of semantic-version comparators (`>=`, `>`, `<=`, `<`,
`=`; a bare version means `=`). `contracts` maps a supported contract name to a
nonempty list of exact positive major versions. This build supports:

- `app-actions`: major 1 (strict declaration validator; no dispatch implied)
- `app-assistant`: major 1
- `app-chat`: major 1
- `app-screens`: major 1
- `app-views`: major 1

A package without `needs.requires` is legacy and remains admitted as before; its
existing structural and bundle digests are unchanged. When requirements are
declared, malformed fields, unknown keys/contracts, unsupported majors, or a
core range that excludes this host refuse validation. Install and upgrade
validate requirements before writing catalog, journal, or record state. The
install-check and upgrade-check receipts, and catalog show description, include
the declaration and this host's core version and supported contract majors.

These requirement rules are implemented once in `crates/cadence-core`, used
directly by native validation and compiled to `wasm32-unknown-unknown` for a
hosted adapter. The bounded JSON command and WASM ABI evaluate requirements;
they do not install packages, grant consent, dispatch work or own business
records. Each adapter supplies its real host version and supported contract
profile. The hosted build pins the canonical native Git revision and verifies
its source/artifact hashes rather than maintaining a second rule engine.

## Offline screen-frame previews and projection fixtures

`cadence app frame-kit --out <dir> [--bundle <local-app-bundle>] [--parent-origin <origin>]`
writes `frame.html`, `kit.css`, and `metadata.json` without contacting the
daemon or reading organization, PM, installation, session, or app state. The
output directory must not already exist. With a bundle, the command validates
it locally and mounts its single matching `app-screens/v2` screen entry and
styles; without one, it emits the empty preview frame. The document uses the
same `render_frame_html` as the live screen RPC, and `kit.css` is the exact host
`SCREEN_KIT_CSS` injected into that document. The default parent origin is
`http://127.0.0.1`.

`cadence app fixtures --bundle <local-app-bundle> --json` validates a local
bundle and emits six deterministic synthetic canonical `PUSH` scenarios:
`empty`, `loading`, `unavailable`, `ready`, `draft`, and `review`. It requires
Node.js on `PATH` to run the bundled trusted producer; app assets are validated
as data and are not executed. The fixture schema is
`app-screen-projection-fixtures/1`. `caption_digest` remains the native-read
HTTP response field and is not added to the canonical `PUSH` fixture shape.

The frame uses offline placeholders for bridge nonce and generation; these are
not a capability, session, receipt, or permission to call the host. The export
and fixtures do not grant app authority or read installation state. During UI
builds, CI regenerates the canonical producer and golden output from the real
Vite dependency graph. Its checked-in baseline records content hashes for
included repository sources, and freshness checks still require the artifact,
golden, and complete dependency graph to match regeneration. A numeric
fixture-contract version bump is required only when the serialized canonical
golden changes from the trusted event base; producer or build-tool changes
that leave that golden identical do not require one. Version downgrades are
always refused. The golden covers these six generic host examples, not every
possible future app-domain input path: contract review and consumer regressions
remain necessary. CI compares against the trusted event base (fetching only
that exact missing base commit in CI) and refuses when the base or its baseline
is unavailable or malformed.
