# Publish Web (CAD-1429)

`cadence web` freezes a plain HTML file or a built static folder on this machine
and asks the selected AgenticOS workspace owner to publish it. Nothing goes live
until the owner approves in the browser. No npm, build or project script runs,
either locally or on the server.

## Select a workspace and grant web scopes

Hosted actions need a selected AgenticOS workspace (`--org`, or the org
selected for the session). Consent for the Publish Web scopes is requested at
login:

```sh
cadence login --web
```

Without `--web` the login requests `cli.read`, `cli.write`, `wiki.read`, and
`wiki.write`, and grants no web authority. With `--web` the scopes requested
are `cli.read`, `cli.write`, `web.read` and `web.prepare`. No `wiki.*` scopes
are requested by `--web`. A `web.prepare` credential can prepare releases and
request publication, rollback or unpublish, but it cannot promote a release or
change the public pointer. Only browser-owner approval does so.

## Commands

The subcommands are those printed by `cadence web --help`:

```sh
cadence web prepare <input> --name <slug> [--spa] [--org <org>]
cadence web publish <release> [--org <org>]
cadence web status <slug> [--org <org>]
cadence web releases <slug> [--org <org>]
cadence web submissions <slug> [--form <key>] [--limit <n>] [--cursor <opaque>] [--org <org>]
cadence web rollback <slug> <release> [--org <org>]
cadence web unpublish <slug> [--org <org>]
```

- `prepare` takes a `.html` file (published as `index.html`) or a built output
  folder such as `dist/`. `--name` is the site slug: 3 to 63 lowercase letters,
  digits or hyphens. `--spa` opts in to SPA fallback, so unmatched navigation
  paths serve `index.html`. It is off by default and part of the owner-approved
  release.
- `publish` asks the owner to approve a prepared release. It prints the action.
  The live URL appears only after a confirmed owner approval.
- `status` shows the site's live state and any action in flight from this
  machine.
- `releases` lists releases newest first.
- `submissions` reads a site's form submissions, newest first, as JSON with
  `nextCursor`. `--form` filters one form key, `--limit` is 1 to 100, and
  `--cursor` continues from a previous page. Requires the `web.read` scope.
- `rollback` and `unpublish` ask the owner for approval. They do not act
  directly.

## Limits

- 2000 files per release, 100 MiB total, 10 MiB per file. The CLI rejects a
  folder above these limits before any upload.
- Private storage settings may only lower the server ceilings.

## Asset naming on updates

A live site's HTML may change in place, but changed bytes at a non-HTML path that
an earlier retained release still uses are refused with `409 asset_conflict`.
Name each asset after its content, for example `assets/<sha256>.css`, and update
the HTML that references it. A path whose last segment starts with the file's
sha256 is served immutable; anything else revalidates with its ETag.

## Owner approval

`publish`, `rollback` and `unpublish` return an approval URL that is valid for 15
minutes. Approval is browser-only: any `Authorization` header is refused, even
with an owner cookie.

- Open the link from a browser signed in as an owner of the selected workspace.
  If that browser has not selected the workspace, the approval read answers 404
  and the page shows "This approval no longer matches".
- Re-running the command while the action is open resumes that action and the
  same link. It does not approve anything.
- Once the action has expired, a new action must be created; the CLI keeps one
  durable intent per release and does not mint a second one on its own.

## Local state and retries

Each request is written as a durable intent before its first side effect. A
later run with the same intent reuses the same request key and body. It never
rebuilds the request from fresh state. A failed or unknown outcome is shown as
such and is not reported as success.

A typed 409 with `ok: false` and `revision_conflict`, or another validated
4xx refusal (except 401/403), closes the old intent as failed. The refused
request is not resent with a fresh key during that invocation; the next explicit
invocation reads the current revision and creates a new key and body. Unknown,
lost, 5xx, authentication refusals and malformed-success outcomes keep the
intent open with its original key and body. When an action id is known, an
explicit action command reads that action first: an active action resumes the
same intent, while a terminal receipt is saved before that invocation creates a
fresh intent. `status` also saves known terminal receipts. A missing auth
directory is refused; only the separate login grant flow creates it.

## Known limits

- Live public serving, public DNS and wildcard routes are not deployed. The
  commands can be exercised against a local rig; a public URL is not claimed.
- Proven by tests: local HTTPS positive lifecycle cases for plain HTML and a
  locally built React static site against a real API with a separate cookie
  owner; library client tests (19 passed, 2 ignored); single-HTML byte total
  unit test; TLS tests (7 passed); routing (3) and refusal (1) acceptance with
  the test seam. Strict clippy was run before the final correction batch and is
  not repeated for it. Not proven: a
  production-built CLI over TLS end to end, or a cold process restart mid-flow.
- Owner browser approval was checked in mock states and at 390px only. Native
  keyboard, reduced-motion and a full browser end-to-end run are not proven.
  Browser checks status: native keyboard, reduced-motion and reconciliation are
  unverified.
