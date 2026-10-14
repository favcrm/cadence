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

Without `--web` the login is unchanged and grants no web authority. The
scopes requested are `web.read` and `web.prepare`. They do not imply
`cli.*` or `wiki.*`. A `web.prepare` credential can prepare releases and
request publication, rollback or unpublish, but it cannot promote a release or
change the public pointer. Only browser-owner approval does so.

## Commands

The subcommands are those printed by `cadence web --help`:

```sh
cadence web prepare <input> --name <slug> [--spa] [--org <org>]
cadence web publish <release> [--org <org>]
cadence web status <slug> [--org <org>]
cadence web releases <slug> [--org <org>]
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
- `rollback` and `unpublish` ask the owner for approval. They do not act
  directly.

## Limits

- 2000 files per release, 100 MiB total, 10 MiB per file. The CLI rejects a
  folder above these limits before any upload.
- Private storage settings may only lower the server ceilings.

## Local state and retries

Each request is written as a durable intent before its first side effect. A
later run with the same intent reuses the same request key and body. It never
rebuilds the request from fresh state. A failed or unknown outcome is shown as
such and is not reported as success.

## Known limits

- Live public serving, public DNS and wildcard routes are not deployed. The
  commands can be exercised against a local rig; a public URL is not claimed.
- Proven by tests: local HTTPS positive lifecycle cases for plain HTML and a
  locally built React static site against a real API with a separate cookie
  owner; library client and TLS tests; strict clippy. Not proven: a
  production-built CLI over TLS end to end, or a cold process restart mid-flow.
- Owner browser approval was checked in mock states and at 390px only. Native
  keyboard, reduced-motion and a full browser end-to-end run are not proven.
  Browser checks status: native keyboard, reduced-motion and reconciliation are
  unverified.
