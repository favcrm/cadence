# External agents doing social management

An operator-provisioned person or AI agent off-host can do Social
Content work through Cadence: discover destinations, run quoted
draft/image flows, fetch receipts — while every send stays a human
approval. This guide is the one written path; inventing another access
shape (shared credentials, the operator secret, a pasted provider
token) is not a shortcut.

## Prerequisites

- A tip build with remote-capable sign-in reviewed (CAD-777) for V2;
  V1 works on any recent build.
- The daemon on its host, loopback-bound, with an enrolled
  `agenticos_external` connection (`provider.read`/`provider.draft`).
- An approved Social Content install with a destination from owner-
  authorized discovery (`cadence connection ls` / `show`), never a
  typed handle or pasted URL.

## V1 access: SSH or tailnet (works today)

1. Provision the agent **one** SSH account (or tailnet identity). No
   shared credentials, no operator secret, no `--token-stdin` values
   ever leaving your own shell.
2. The agent reaches the CLI over SSH and the board over an `ssh -L`
   tunnel or `cadence ui run --tailscale` (see `BOARD.md` remote
   access). The daemon socket stays local; no new listener.
3. The agent joins as a Cadence agent (`cadence join <pm> <provider>`)
   so every action carries its alias — or arrives with an enrolled
   endpoint identity. Unattributable callers get nothing (see
   pre-flight).

## Grants: least privilege, derived from the app

There is no standing "agent may do social" permission. Authority
derives per install and per run:

```sh
cadence app set-team <install> writer=<alias> reviewer=<other-alias>
cadence app catalog approve <install> --digest <installed-digest>
```

Approval derives exactly the scopes the workflow steps declare on
their bound slots, for exactly the teamed agents (CAD-577). Writer
and reviewer must differ; the same alias never holds both roles on
one run. Agents get no grant/admin verbs: no `connection
create/rotate/revoke`, no `app approve/revoke`, no grant widening.

## Allowed vs forbidden

An agent may: list/show/check connections, install-read/catalog-read
app state, create quoted runs, produce drafts, fetch receipts and
retained artifacts, report results.

An agent may never: approve its own (or any) run, release a send,
select or alter a destination, mint a session, rotate/revoke
anything, or approve a schedule. Sends execute only after the
operator's digest-pinned approval in the board (CAD-771); revocation
of the agent (grants + access removal) closes its pending effects
through the custody lifecycle.

## Pre-flight: prove attribution before first work

Run from the agent's account, before granting anything:

1. A read the agent will keep, e.g.
   `cadence connection show <id>` — must succeed.
2. An operator-only call, e.g. `cadence app approve <install>
   --digest <d>` — must be refused, and the refusal must name the
   **agent alias**, never `operator`.
3. Confirm the daemon attributes the agent's session correctly
   (`cadence ui sessions` shows origin; no device/link session may
   exist for the alias).

Fail-open attribution — anything attributed to `operator` that is
not the operator, or any operator-only success — stops the
onboarding. That is a security finding, not a docs gap.

## V2: device-grant sign-in (pending CAD-777 + AOS-97)

When the device-login routes land, replace step V1.1 with board
sign-in (`--device-login-issuer/org`, owner-approved grant, 12 h/24 h
session); SSH becomes fallback only. Unchanged: alias-carried
actions, derived grants, human-approved sends.

Explicit non-goal of this guide: scoped API/MCP agent bearers for
headless agents. That is a separate ticket with its own adversarial
tests — this guide's agents are interactive board/CLI users.

## Revocation

Remove the alias from the app team (or revoke approval), remove its
grants, close its access. Verify: its sessions are gone from
`cadence ui sessions`, its pendings settle expired, and its next
call is refused as an unknown/ungranted caller.
