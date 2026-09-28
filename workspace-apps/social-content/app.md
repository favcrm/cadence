---
app: social-content
title: Social Content
version: '0.3.0'
summary: Choose a retained public Instagram source or paste facts, review a zh-HK caption, then explicitly release to Local.
needs:
  connections: []
  capabilities:
    source:
      schema: 1
      capability: social.read
      version: 1
      action: list_posts
      resource_kind: connection_account
      effect: read
    publication:
      schema: 1
      capability: text.publish
      version: 1
      action: publish
      resource_kind: connection_account
      effect: send
---

# Social Content — reviewed captions

Choose Instagram or Facebook and make one caption per run from source
facts. You can paste the facts or choose one retained public Instagram
post through the `source-instagram` acquisition workflow. This workspace
bundle needs no project. The channel is a writing brief; releasing a
caption delivers it to Local, not that social network. Image generation,
scheduling and external posting remain unavailable in this version.

## Set up once

From the Cadence source checkout, install this directory and read the
returned installation ID and bundle digest. Approve that exact digest:

```sh
cadence app catalog install workspace-apps/social-content
cadence app catalog approve INSTALL_ID --digest BUNDLE_DIGEST
```

Use two different registered workers in the same registered PM group:
a writer and a reviewer. Supply their aliases and the owner PM on every
run; brand settings never select workers or grant provider authority.

For repeated work with a client, optionally create an app context with
`brand_voice` and `protected_terms` string defaults. Both are explicitly
eligible content defaults in both caption workflows. The source is always a
new run input. A context is optional; omitting it creates a context-free run.
Brand guidance and protected terms are quoted content, never commands
that can override factual fidelity, the rubric or the result contract.

To allow a later Local release, list connections and create a
`publication` binding before creating the run. Use the exact returned
connection ID:

```sh
cadence connection ls
cadence app binding create INSTALL_ID --slot publication --connection-id CONNECTION_ID --request-id bind-social-local
```

When using a brand context, add `--context-id CONTEXT_ID` to the binding
command and the run command. Follow `docs/guides/app-artifact-release.md`;
a binding added after a run
was frozen cannot authorize that old draft. Drafting without a binding
is allowed but that run cannot subsequently release.

## Read a public Instagram source

The separate `agenticos_external` connection is an operator-enrolled,
company-scoped AgenticOS device credential with `provider.read` audience.
The provider keeps the Treg key and determines the company from the device
credential. The operator must bind the installation's `source` capability
to that connection, in the same context used for the later caption run.
The bound provider action is the reviewed public Instagram posts read; no
worker can choose another provider, account, profile, tool or effect.

Create and approve a `source-instagram` run with a `profile_handle`
selected by the operator, then dispatch it. The assigned worker invokes
the run-bound `source` capability and returns the resulting receipt ID.
The broker retains normalized posts and their origin. It never treats a
worker's copied text as a source receipt. Only the operator can choose one
post ID from that receipt for a caption run in the same installation and
context. A private, missing, rate-limited, deleted or malformed source
returns a visible refusal or an honest empty result; it never becomes
sample content. A profile with no posts cannot verify its identity.

For the caption run, include its `source_receipt_id` and
`selected_post_id` and omit `inputs.source`. The server freezes the
byte-exact selected caption and receipt digest, then inserts a derived
single-line `source` input for the text workflow. A later rebind, revoke
or different context cannot authorize an old receipt. A preview image URL
is a display hint, not a retained image asset.

## New post

Choose the `instagram` or `facebook` workflow. Each fixes its channel
in the workflow; there is no comma-separated destination input. For both
channels, create a separate run so each caption has its own review and
release receipt.

Supply `subject`, `source`, `writer` and `reviewer`. `brand_voice` and
`protected_terms` are optional. Inputs currently require a single line:
paste source facts as plain text with spaces between original lines,
retaining exact names, prices, URLs, claims and disclaimer wording. A
URL alone is not source material; paste the actual facts. Newlines and
control characters are refused by the workflow parser. A subject is a
short label, not a folder or output path. Keep source and brand guidance
concise enough to fit each bounded 16 KiB step instruction; oversized
inputs refuse instead of silently truncating.

For example, a context-free Instagram input JSON file is:

```json
{
  "subject": "Summer ramen",
  "source": "Kura Summer Ramen HK$88. Available 1–30 July. 每日限量 40 碗。優惠受條款及細則約束。",
  "writer": "op-social-writer",
  "reviewer": "op-social-reviewer",
  "brand_voice": "Short, warm, clear zh-HK captions",
  "protected_terms": "Kura Summer Ramen; HK$88; 優惠受條款及細則約束。"
}
```

Save that JSON in your own input file and substitute the registered
aliases. Create a run, then inspect the returned run ID and snapshot
digest before approving and dispatching:

```sh
cadence app run create INSTALL_ID --workflow instagram --inputs INPUTS_JSON --owner-pm OWNER_PM --request-id UNIQUE_REQUEST_ID
cadence app run show RUN_ID
cadence app run approve RUN_ID --digest SNAPSHOT_DIGEST
cadence app run dispatch RUN_ID
```

Optionally add `--context-id CONTEXT_ID` to create. Use `--workflow
facebook` for Facebook. The writer returns one run-owned text
artifact through the authenticated result envelope supplied in its
kickoff. The independent reviewer fetches that exact artifact with its
active message and turn token and returns `approve` or `revise` pinned to
the artifact SHA256 digest. A rejected caption stays failed; create a new
run with corrected source or guidance rather than claiming release.

## What good looks like

Written Hong Kong Chinese: a written-language structure with Cantonese
rhythm, short sentences, one idea per line, at most one emoji. Preserve
source brand names, product names, currency, prices, dates, URLs,
protected hashtags and disclaimers exactly. No invented facts,
superlatives, regulated claims or promises. A caption carrying a price
or a health, financial or legal claim also carries the source's related
disclaimer unchanged. Missing or conflicting required facts or
unsupported protected terms require a concrete review rejection.

The complete review checklist is included in both review instructions
and `rubrics/brand.md`. Only the caption belongs in the text artifact:
no headings, verdict, source dump, image brief or extra channel caption.
This version intentionally uses a concise 2,200-character and 8 KiB
UTF-8 caption ceiling for either channel. It is an app limit, not a claim
about Facebook's maximum. Hashtags go last, at most three, protected
ones first. If faithful facts/disclaimers cannot fit, reject rather than
cutting them off.

## Explicit release

A successful review is not publication. Inspect the accepted artifact
and stage its exact ID on `publication`. Read the complete effect
preview and approve its exact effect digest to release once to Local:

```sh
cadence app effect stage RUN_ID --artifact-id ACCEPTED_ARTIFACT_ID --slot publication --request-id UNIQUE_RELEASE_ID --title 'Reviewed Instagram caption'
cadence app effect show EFFECT_ID
cadence app effect accept EFFECT_ID --digest EXACT_EFFECT_DIGEST
```
No publisher worker, project name, file path, legacy grant or
`platform_call` is needed. Neither worker stages or accepts a send.
Declining sends nothing. If an outcome is uncertain, inspect its receipt
and use operator reconciliation; never automatically repeat the release.
The Local outbox item proves Local delivery only.
