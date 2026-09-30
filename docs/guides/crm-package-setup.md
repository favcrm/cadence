# CRM package setup

`workspace-apps/crm` is the installable CRM metadata bundle. Installing it
creates a workspace installation whose app is named `crm` — the name the
host-compiled CRM screens (Customers, Segments, Campaigns, content review,
bounded send controls) key on. The package is metadata plus one local-only
`email-brief` workflow; the CRM UI, customer records domain, authorization
checks and sending are **host-compiled features of the daemon and board**,
not contents of this bundle. Installing it grants no additional authority —
it makes the already host-compiled UI discoverable to an authorized operator
under the name `crm`; it does not itself confer UI access, records, SMTP
custody, send authority, or a sandbox over what workers may do.

Use a trusted Cadence source checkout and an owned, disposable test daemon
and workspace. These are operator commands, not a production rollout.

## Install and approve

```sh
cadence app catalog install workspace-apps/crm
cadence app catalog show <install-id>
cadence app catalog approve <install-id> --digest <installed-digest>
```

`install` validates and copies the local bundle and returns a stable
installation ID plus the bundle `digest` and `catalog_generation`.
`approve` requires the exact returned digest; it permits the bundle's
bounded local text capability only. It creates no run, dispatch, binding,
customer record, SMTP connection or send authority.

## Optional context

A context groups content defaults and run history inside one installation.
`email-brief` declares `brand_voice` and `protected_terms` as
`context_default` inputs — content-only strings a context may carry.

Write a JSON file of string defaults, for example `defaults.json`:

```json
{"brand_voice": "Calm, plain English", "protected_terms": "HK$88/month; 1 July"}
```

Then:

```sh
cadence app context create <install-id> --label "Default voice" --defaults ./defaults.json --request-id crm-context-1
cadence app context ls <install-id>
cadence app context show <install-id> <context-id>
```

`--defaults` takes a **file path**, not inline JSON; omit it for an empty
default map. A context is optional — omit `--context-id` on the run for a
context-free run.

## Synthetic customer records

For package testing use synthetic records only — `example.invalid`
addresses, `consent.email: "unknown"`, and a `demo` tag so nothing is ever
mistaken for a real customer:

```sh
cadence app record create <install-id> --context-id <context-id> \
  --record-id customer-a --profile ./customer-a.json
cadence app record ls <install-id> --context-id <context-id>
cadence app record show <install-id> --context-id <context-id> --record-id customer-a
```

`--profile` is a JSON file holding the customer profile object, e.g.
`{"schema":1,"display_name":"Amina Diallo","email":"amina@example.invalid","tags":["demo"],"consent":{"email":"unknown"}}`.

## Run `email-brief`

The workflow needs two **different registered workers in the same owner PM
group**: a writer and an independent reviewer. Register them under the
daemon's PM and pass their aliases as inputs; the bundle neither selects
workers nor grants provider authority.

Put run inputs in a JSON file of strings (`--inputs` takes a file path,
bounded to 32KiB), e.g. `inputs.json`:

```json
{
  "subject": "Spring launch follow-up",
  "audience": "Customers due a renewal reminder",
  "facts": "Plan renews 1 July. Price stays HK$88/month. Reply to change plan.",
  "writer": "op-crm-writer",
  "reviewer": "op-crm-reviewer"
}
```

Then create, inspect, approve and dispatch the run:

```sh
cadence app run create <install-id> --workflow email-brief \
  --inputs ./inputs.json --owner-pm <registered-pm> --request-id crm-brief-1
cadence app run show <run-id>
cadence app run approve <run-id> --digest <snapshot-digest>
cadence app run dispatch <run-id>
```

Pass `--context-id <context-id>` on `run create` to apply context defaults.
The writer returns one run-owned text artifact through the authenticated
result envelope; the reviewer independently verifies its digest and answers
`approve` or `revise` pinned to it.

## What the package does not do

A successful `email-brief` review records **reviewed draft text only**. It
is not campaign content, not an approved campaign, and not a send. The
following are separate host-compiled operator actions on the installation —
each gated by the daemon's own authorization and approvals — and remain so
regardless of what any bundle text asks for:

- saving and approving campaign content (the host's content-review
  surface, not a bundle step),
- preparing and freezing an audience (`cadence app audience … prepare`),
- enrolling and binding an SMTP connection (`cadence connection create`,
  then binding it to the installation context on the board/RPC surface),
- the bounded test send and the campaign send approval (host operator
  actions on the CRM campaign screens and their RPCs — not CLI verbs the
  bundle supplies).

SMTP credentials, sender identity and all sends stay under host operator
custody. The package carries no SMTP enrollment, performs no send, and
declares no capability that could carry one.

## Session, SSH and proof expectations

The privileged operator verbs above — install, approve, upgrade, context and
record writes — authorize through the daemon's operator peer proof on the
calling connection; over HTTP the board additionally binds a browser session.
The registered `writer`/`reviewer` workers receive only their specifically
delegated run-result and artifact-fetch verbs, not operator authority.
Operating over an SSH login to the host does not weaken or bypass any of
this; do not loosen auth, allowlists or the test seam to "handle" SSH. For
privileged actions, agent callers, detached children and forged fields are
refused by the daemon, not by the bundle.

## Upgrade boundary

A package-only upgrade (changed `version`/content bytes) goes through the
supported check/commit pair:

```sh
cadence app catalog upgrade-check <install-id> --source <dir> \
  --expected-digest <current-digest> --expected-generation <generation>
cadence app catalog upgrade <install-id> --source <dir> \
  --expected-digest <current-digest> --expected-generation <generation> \
  --expected-new-digest <proposed-digest> --request-id <unique-id>
```

The upgrade preserves installation, context and record identities and
leaves the new digest unapproved until `app catalog approve` runs on it.
It performs **no schema or arbitrary data migration** — it only swaps the
bundle bytes under the same identity, with CAS refusal on a stale expected
digest.

## Production boundary

Everything above describes package installation and local, package-tested
behaviour on a disposable workspace. The host-compiled CRM UI and send path
are verified separately; installing or approving this package is **not**
proof of the production send path, and a host binary rollout is a separate
operator-owned action.
