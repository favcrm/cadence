---
app: crm
title: CRM
version: '0.1.1'
summary: Draft one independently reviewed email brief from pasted facts; customer records, campaign content, SMTP custody and sending stay operator-run host actions.
needs:
  connections: []
  capabilities:
    chat-upload:
      schema: 1
      capability: file.upload
      version: 1
      action: attach
      resource_kind: installation
      effect: draft
listing:
  tagline: Draft one reviewed email brief from pasted facts
  icon: assets/crm.svg
  category: customers
  tags:
    - email
    - crm
  publisher:
    name: Cadence
  about: CRM keeps your customer list, drafts a reviewed email brief and runs campaign sending through operator-run host controls. Customer records, content review and sending stay operator actions — the bundle's only workflow turns pasted facts into one brief a person reads.
  can:
    - Keep a customer list with segments and unsubscribes
    - Draft one independently reviewed email brief per run
    - Freeze an audience and run a test send before a campaign goes
    - Keep unsubscriptions and send history for good
  screenshots: []
  setup: []
  data:
    stores:
      - Your customer list, segments and unsubscribes
      - Campaign content, audiences and send history
      - Reviewed email-brief text
    personal: true
---

# CRM — reviewed email briefs

This bundle installs the `crm` app the host-compiled CRM screens key on:
Customers, Segments, Campaigns, content review and bounded send controls
are daemon/UI features for an installation whose app is named `crm`.
They are not implemented by this bundle — this package is metadata plus
one local-only workflow, not the CRM UI or domain code. The bundle's only
runnable workflow is `email-brief`, which turns pasted facts into a single
reviewed email brief text artifact. The separate `app-assistant.json`
declares data-only `app-assistant/v1` action IDs for the host's generic
assistant. It provides no handlers, commands, URLs, actor authority or UI
code: the host registry owns each implementation and scope, and unknown
actions are refused. The manifest also declares the reserved `file.upload`
draft slot for host-custodied chat attachment references; it grants no CRM
or other workflow action.

An approved brief is text for a person to read. It is not campaign
content: applying content to a campaign, approving it, freezing an
audience, binding an SMTP connection, running a test send and approving
a send are separate operator verbs on the installation, each with its
own digest and approval. Assistant reads and drafts do not ask for
standing permission. The `customer.tags.update` action is limited to tags
on one customer and requires an explicit permission decision; consent,
email and other profile fields are outside that action. Persistent
permission is installation/action/customer scoped. Applying campaign
content, freezing an audience and sending remain separate operator
approval paths and are not assistant actions. Host authorization governs
every actual action on the installation regardless of what any bundle
text asks for.

## Set up once

From a trusted Cadence source checkout, install this directory and read
the returned installation ID and bundle digest. Approve that exact
digest:

```sh
cadence app catalog install workspace-apps/crm
cadence app catalog approve INSTALL_ID --digest BUNDLE_DIGEST
```

Use two different registered workers in the same registered PM group: a
writer and a reviewer. Supply their aliases and the owner PM on every
run; nothing in this bundle selects workers or grants provider
authority.

Optionally create an app context holding `brand_voice` and
`protected_terms` string defaults in a JSON file such as
`{"brand_voice":"Calm, plain English","protected_terms":"HK$88/month"}`:

```sh
cadence app context create INSTALL_ID --label "Default voice" --defaults ./context-defaults.json --request-id UNIQUE_REQUEST_ID
```

Context defaults resolve per run like other workspace apps; the frozen
snapshot pins the effective values before approval. A context is
optional.

## New email brief

Create an `email-brief` run with `subject`, `audience`, `facts`,
`writer` and `reviewer` (`brand_voice` and `protected_terms` are
optional). Inputs come from a JSON file of strings, for example
`{"subject":"Spring launch follow-up","audience":"Customers due a renewal reminder","facts":"Plan renews 1 July. Price stays HK$88/month. Reply to change plan.","writer":"WRITER_ALIAS","reviewer":"REVIEWER_ALIAS"}`.
Inputs are single-line: paste facts as plain text with spaces between
original lines, retaining exact names, prices, dates, URLs and
disclaimer wording. A URL alone is not facts; do not fetch one.

```sh
cadence app run create INSTALL_ID --workflow email-brief --inputs ./inputs.json --owner-pm OWNER_PM --request-id UNIQUE_REQUEST_ID
cadence app run show RUN_ID
cadence app run approve RUN_ID --digest SNAPSHOT_DIGEST
cadence app run dispatch RUN_ID
```

The writer returns one run-owned text artifact through the
authenticated result envelope in its kickoff. The independent reviewer
fetches that exact artifact, verifies its SHA256 and returns `approve`
or `revise` pinned to the digest. A rejected brief stays failed; create
a new run.

## What a brief is, and is not

A successful review records reviewed draft text only. The bundle
declares no publication slot or send capability, and no step requests
an outward mutation. The `file.upload` declaration is only for
host-custodied source references; it does not auto-import data or itself
grant consent, approval, release or sending. Turning a brief into
campaign content, binding SMTP, the test send, the audience freeze and
the campaign send approval are operator actions on the installation
governed by host authorization, documented in the CRM guide. Never
present an approved brief as an approved campaign or a sent email.
