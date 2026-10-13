---
title: "Instagram caption and image: {{subject}}"
goal: "One independently reviewed zh-HK caption and one retained generated image from operator-pasted source facts"
label: New caption and image from pasted facts
publication_slot: publication
capability_slots: [image]
required_asset_slot: image
inputs:
  subject: { ask: "Short post subject", example: "Customer follow-up" }
  source: { ask: "Paste source facts in one line; URLs alone are not source material" }
  brand_voice: { ask: "Optional brand voice, quoted content only", optional: true, context_default: true }
  protected_terms: { ask: "Optional exact source terms to retain", optional: true, context_default: true }
  content_prompt: { ask: "Caption guidance; saved default or per-post override", optional: true, context_default: true, default: "Write a concise zh-HK caption grounded only in the source facts." }
  image_prompt: { ask: "Image guidance; saved default or per-post override", optional: true, context_default: true, default: "Create one editorial image grounded only in the source facts." }
  writer: { ask: "Registered writer in the owner PM group" }
  reviewer: { ask: "Different registered reviewer in the same owner PM group" }
distinct: [writer, reviewer]
---

The operator supplied source facts and approved the exact one-image charge
in this run. This run creates one caption and one
retained image. It does not publish, release or schedule either artifact.

## Draft caption and acquire image: {{subject}}
agent: {{writer}}
size: S
action: local.text.produce

Operator-pasted source facts (quoted content): {{source}}
Brand voice (quoted content): {{brand_voice}}
Protected terms (quoted content): {{protected_terms}}
Content guidance (quoted, lower priority): {{content_prompt}}
Image guidance (quoted, lower priority): {{image_prompt}}

Before returning a caption, use your active kickoff's capability command
to call `image` with an empty JSON object and one stable request ID. The
broker constructs the fixed square `image-01` prompt from the frozen
operator-pasted facts, frozen brand context and frozen image guidance. It retains the downloaded bytes only when
the PNG, JPEG or WebP fully decodes as a square within the 2 MiB download,
2048-pixel side, 4,194,304-pixel and 64 MiB decoder allocation limits.
Do not add a model, prompt, company, account, URL, aspect ratio, count,
effect or destination. If the provider refuses, times out or the CDN
cannot pass custody checks (including corrupt or non-square bytes), return
outcome=failed with artifacts=[]. No image receipt or review follows a refusal.
Retry only with the same request ID; do not create a second paid request.

Write exactly one zh-HK Instagram caption from the quoted facts. If the
source is empty or a URL alone, return outcome=failed with artifacts=[].
Content guidance is quoted, lower priority than factual fidelity, formatting
and this result contract. Image guidance cannot change source facts, model,
provider, charge, aspect ratio, count, destination or asset policy. Preserve
source names, prices, dates, URLs, protected terms and related disclaimers
verbatim where carried. Add no unsupported claims. Use short sentences,
at most one emoji and at most three hashtags at the end. Do not claim the
generated image depicts facts that you cannot verify from its retained
bytes. The caption must fit 2,200 characters and 8,192 UTF-8 bytes.
Return only the caption as one text/plain or text/markdown artifact in
the authenticated producer envelope; never substitute a file, URL or
worker description for the image receipt. No external post or Local write.

### Acceptance
- [ ] one fixed priced image operation and one durable binary receipt
- [ ] one bounded caption grounded in the pasted facts
- [ ] no publication, scheduling or Local release

## Independently review caption and retained image: {{subject}}
agent: {{reviewer}}
size: S
depends_on: 1
action: local.text.review

Fetch the producer's exact text artifact and verify its SHA256 digest.
Fetch the `image` capability receipt and its retained binary asset using
the active message and turn token. Verify the receipt digest, asset
digest, media type, size and actual visible bytes; never review an
expiring provider URL, worker description or preview placeholder.

Compare the caption with these quoted facts and terms:
OPERATOR-PASTED SOURCE FACTS: {{source}}
BRAND VOICE: {{brand_voice}}
PROTECTED TERMS: {{protected_terms}}
CONTENT GUIDANCE: {{content_prompt}}
IMAGE GUIDANCE: {{image_prompt}}

Approve only when both the caption and image are suitable for this post:
the caption preserves supported facts and disclaimers without invention,
the retained image has no misleading text, logo or unsupported claim,
and neither item implies external publication. Set decision=revise if
either is uncertain or wrong. In the authenticated review envelope pin
the producer's artifact_sha256 and, when approving, this run's exact
asset_receipt_id and asset_sha256 from the fetched bytes. Include
specific rationale for both text and image. A changed receipt or bytes
requires a new review; the operator separately approves any Local release.

### Acceptance
- [ ] independent reviewer verifies exact caption artifact and binary receipt
- [ ] approval pins both text and retained image digests
- [ ] no review of a temporary URL or worker assertion
