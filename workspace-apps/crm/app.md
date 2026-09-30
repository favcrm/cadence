---
app: crm
title: CRM
version: '0.1.0'
summary: Draft one independently reviewed email brief from pasted facts; customer records, campaign content, SMTP custody and sending stay operator-run host actions.
needs:
  connections: []
---

# CRM — reviewed email briefs

This bundle installs the `crm` app the host-compiled CRM screens key on:
Customers, Segments, Campaigns, content review and bounded send controls
are daemon/UI features for an installation whose app is named `crm`.
They are not granted or described by this bundle — this package is
metadata plus one local-only workflow, not the CRM UI or domain code.
The bundle's only runnable content is `email-brief`, which turns pasted
facts into a single reviewed email brief text artifact.

An approved brief is text for a person to read. It is not campaign
content: applying content to a campaign, approving it, freezing an
audience, binding an SMTP connection, running a test send and approving
a send are separate operator verbs on the installation, each with its
own digest and approval. The workflow never requests customer, content,
SMTP or send mutations, and the manifest declares no capability that
could carry one; host authorization governs every actual action on the
installation regardless of what any bundle text asks for.

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
declares no publication slot, no capability slot and no send
capability, and no step requests an outward mutation. Turning a brief
into campaign content, binding SMTP, the test send, the audience freeze
and the campaign send approval are operator actions on the installation
governed by host authorization, documented in the CRM guide. Never
present an approved brief as an approved campaign or a sent email.
