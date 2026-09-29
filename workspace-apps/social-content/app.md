---
app: social-content
title: Social Content
version: '0.5.0'
summary: Draft a reviewed zh-HK caption and optional image from retained Instagram or pasted facts with per-post content and image guidance before Local release.
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
    image:
      schema: 1
      capability: media.generate
      version: 1
      action: generate_image
      resource_kind: connection_account
      effect: draft
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
caption delivers it to Local, not that social network. An image-enabled
caption run can retain one generated asset from a selected source.
Scheduling and external posting remain unavailable in this version.

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
`brand_voice`, `protected_terms`, `content_prompt` and `image_prompt` string
defaults. The two prompts also have app defaults. An empty per-post prompt
field resolves to the saved context value, then the app default. A nonempty
field overrides only that post. The effective values and their origin are
frozen in its run snapshot before approval; later context edits cannot
rewrite that run. Prompt guidance is bounded to one visible line, 512
characters and 2,048 UTF-8 bytes. The source is always a new run input or
a selected receipt, never a context default. A context is optional.
All guidance is quoted content below factual fidelity, formatting, image
safety, the rubric and the result contract.

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
sample content. An empty page verifies the profile only when the provider
returns a matching public account identity.

For the caption run, include its `source_receipt_id` and
`selected_post_id` and omit `inputs.source`. The server freezes the
byte-exact selected caption and receipt digest, then inserts a derived
single-line `source` input for the text workflow. A later rebind, revoke
or different context cannot authorize an old receipt. A preview image URL
is a display hint, not a retained image asset.

## Generate one image from a retained post or pasted facts

An operator may bind the `image` capability to a company-scoped AgenticOS
device connection with `provider.draft` scope. This binding requires a
matching manifest pin in root-owned provider deployment metadata; without
that pin, image price discovery stays closed. The approval screen shows
the current per-image rate read from AgenticOS; the run is billed at the
provider's actual charge, and a rate that changed since approval refuses
at dispatch.

Create an `image-instagram` run using a selected source receipt and post in
the same installation/context. For operator-pasted facts, create an
`image-manual` run with `inputs.source` and no receipt. Both require a short
subject, writer and independent reviewer. A URL alone is not source facts.
The run freezes source facts, effective content and image prompts, brand
voice and the current one-image charge.
The operator approves that exact run and cost before dispatch. The worker
calls the `image` capability with an empty object; it cannot choose the
provider, model, image count, image guidance, aspect ratio or destination. The
broker retains only a PNG, JPEG or WebP of at most 2 MiB that fully decodes
to a nonzero square no wider than 2048 pixels (at most 4,194,304 pixels),
within a 64 MiB decoder allocation budget. Corrupt, non-square, oversized
or MIME-mismatched bytes are refused without an image receipt; they cannot
reach review or Local release. The independent reviewer checks the caption
and exact retained bytes in this run, pinning both digests. A later Local
draft may cite only that reviewer-pinned asset through the CAD-713 release
gate. Do not release from a temporary provider URL or unreviewed worker
description.

## New post

Choose the `instagram` or `facebook` workflow. Each fixes its channel
in the workflow; there is no comma-separated destination input. For both
channels, create a separate run so each caption has its own review and
release receipt.

Supply `subject`, `source`, `writer` and `reviewer`. `brand_voice`,
`protected_terms`, `content_prompt` and `image_prompt` are optional.
The image prompt applies only to image-enabled workflows. Inputs require a single line:
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
