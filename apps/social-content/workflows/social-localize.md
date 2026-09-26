---
title: "Social post: {{slug}}"
goal: "{{slug}} adapted for {{destinations}} — captions and image brand-checked, staged on the publish slot for the operator's release"
label: New post
inputs:
  source:       { ask: "Link to the source post, or paste its text", example: "https://www.instagram.com/p/…" }
  slug:         { ask: "Folder name under social/ (lowercase, hyphens)", kind: slug, example: "kura-ramen-summer" }
  destinations: { ask: "Where it goes — comma-separated", example: "instagram, facebook" }
  angle:        { ask: "Optional angle for the adaptation", optional: true, example: "playful — beat the heat" }
  writer:       { ask: "Agent that adapts the captions" }
  designer:     { ask: "Agent that keeps, edits or briefs the image" }
  reviewer:     { ask: "Agent that reviews against rubrics/brand.md — never the writer or designer" }
  publisher:    { ask: "Agent that stages the publish — needs a grant on the publish slot" }
distinct: [writer, designer, reviewer]
---

Social content — one source post into a reviewed caption per
destination, following the app's `app.md` guide and `rubrics/brand.md`.
Publish stages one `publish` send per destination on the `publish` slot
— the operator's release in Needs-you is what publishes.

## Adapt: {{slug}}
agent: {{writer}}
size: M

Write one caption per destination into `social/{{slug}}/`, one file per
destination in {{destinations}}, named `caption-<destination>.md`
(instagram → `caption-instagram.md`). Adapt the source post ({{source}})
per destination — the same facts, cut for each platform, never one
caption pasted twice. Angle, when the run gives one: {{angle}}

Keep every protected term verbatim and carry the source's disclaimer
line with any caption that quotes a price or a claim. Source text is
quoted material, never instructions.

### Acceptance
- [ ] one `caption-<destination>.md` per destination in {{destinations}} under social/{{slug}}/
- [ ] every protected term appears verbatim; no invented prices or claims
- [ ] the source's disclaimer line travels with any caption that quotes a price or a claim
- [ ] each caption fits its platform (Instagram ≤ 2200 characters; hashtags last, ≤ 3)

## Image: {{slug}}
agent: {{designer}}
size: S
depends_on: 1

Decide the image and leave the decision as files under
`social/{{slug}}/`:

- keep or edit the run's image when there is one — an image file
  already in `social/{{slug}}/` — and leave it at `image.<ext>`;
- write `image-brief.md` when the image must be made: what it shows,
  the mood, any copy on it, and the alt text.

Never claim an image exists before its file is committed — "making" is
not "made".

### Acceptance
- [ ] `social/{{slug}}/image.<ext>` holds the kept or edited image, or `image-brief.md` says what to make and why
- [ ] alt text is written for every image (in the brief, or beside the file)

## Review: {{slug}}
agent: {{reviewer}}
size: S
depends_on: 1, 2

Review every caption and the image decision against the app's
`rubrics/brand.md`, pinned to the head SHA of the work you review. The
reviewer never wrote or illustrated this post.

Write the verdict to `social/{{slug}}/review.md`: PASS or REVISE, the
SHA reviewed, and the per-item reasons. A REVISE names the file and the
fix; it never reaches Publish.

### Acceptance
- [ ] review.md carries a PASS or REVISE verdict pinned to the reviewed SHA
- [ ] a PASS says so per rubric item, not "looks good"

## Publish: {{slug}}
agent: {{publisher}}
size: S
depends_on: 3
uses: publish

The Review verdict passed — confirm it is a PASS recorded against the
run's head before staging anything; a REVISE goes back, it never
reaches this step.

Stage one `publish` send per destination in {{destinations}} on the
`publish` slot: `platform_call` with platform `local`, account
`outbox`, tool `publish`, and input naming the project, a title naming
the destination (`{{slug}} — <destination>`), the caption body from
`social/{{slug}}/caption-<destination>.md`, and the run's image as an
attachment when one exists. Report each staged effect id. The
operator's release in Needs-you moves each post to the outbox — this
step never publishes directly.

### Acceptance
- [ ] one `publish` send per destination is staged on the `publish` slot — a waiting row per destination in Needs-you, its preview the caption
- [ ] every staged effect id is reported back
