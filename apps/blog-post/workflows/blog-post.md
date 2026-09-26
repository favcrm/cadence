---
title: "Blog post: {{topic}}"
goal: "A reviewed post about {{topic}}, staged on the publish slot for the operator to release"
label: New post
inputs:
  topic:      { ask: "What should the post be about?", example: "How we onboard a new client" }
  slug:       { ask: "Folder name under posts/ (lowercase, hyphens)", kind: slug }
  keyword:    { ask: "Main search phrase", optional: true }
  strategist: { ask: "Agent that researches and writes the brief" }
  writer:     { ask: "Agent that writes the post" }
  designer:   { ask: "Agent that makes the images" }
  reviewer:   { ask: "Agent that reviews — never the writer or designer" }
  publisher:  { ask: "Agent that stages the publish — needs a grant on the publish slot" }
distinct: [writer, designer, reviewer]
---

Blog post pipeline — the CAD-494 pilot (favcrm/cadence-site-content)
packaged as an app. One post per PR, reviewed against
`rubrics/blog.md`; the Publish step stages the `publish` send on the
`publish` slot after the Review verdict passes — the operator's
release in Needs-you is what publishes.

## Brief: {{topic}}
agent: {{strategist}}
size: S

Research {{topic}} (search phrase: {{keyword}}) and competitors; write
posts/{{slug}}/brief.md from the app's `templates/brief.md` with
sources.

### Acceptance
- [ ] brief.md names audience, angle, keywords and at least three sources with links
- [ ] no invented numbers; every claim has a source

## Draft: {{topic}}
agent: {{writer}}
size: M
depends_on: 1

Write posts/{{slug}}/post.md from the brief, following
`templates/post.md`. Keep product names and protected terms exactly as
written in the brief.

### Acceptance
- [ ] post.md follows brief.md; every factual claim is sourced
- [ ] markdownlint and link check pass in CI

## Images: {{topic}}
agent: {{designer}}
size: S
depends_on: 1

Make a hero image and a social image in posts/{{slug}}/images/.

### Acceptance
- [ ] two images, each ≤ 1 MB, each with alt text referenced from post.md

## Review: {{topic}}
agent: {{reviewer}}
size: S
depends_on: 2, 3

Review the PR against `rubrics/blog.md`, pinned to its head SHA. The
reviewer never wrote or illustrated this post.

### Acceptance
- [ ] a PASS or REVISE verdict is recorded against the reviewed head, with reasons

## Publish: {{topic}}
agent: {{publisher}}
size: S
depends_on: 4
uses: publish

The Review verdict passed — confirm it is a PASS recorded against the
PR head before staging anything; a REVISE goes back, it never reaches
this step.

Stage the `publish` send on the `publish` slot: `platform_call` with
platform `local`, account `outbox`, tool `publish`, and input naming
the project, the post's title, the rendered body of
posts/{{slug}}/post.md, and its images as attachments. Report the
staged effect id. The operator's release in Needs-you moves the post
to the outbox — this step never publishes directly.

### Acceptance
- [ ] the `publish` send is staged on the `publish` slot — a waiting row in Needs-you, its preview the rendered post
- [ ] the staged effect id is reported back
