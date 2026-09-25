---
workflow: social-localize
title: "Localise and schedule social posts"
inputs:
  posts: { from: action.selection, type: "list<source_post>" }
each: item in inputs.posts
creates: post
limits: { max_items: 20, max_parallel: 5 }
---

## Adapt the caption
agent: writer
writes: post.caption

Write a zh-HK caption from {{item}}, keeping protected terms verbatim. When the
caption quotes a price, carry the saved disclaimer too. Read the post's
effective instructions, not the defaults, when it has its own.

### Acceptance
- [ ] caption keeps every protected term verbatim
- [ ] caption includes settings.disclaimer when it quotes a price
- [ ] caption ≤ 2200 chars

## Visuals
agent: designer
depends_on: 1
writes: post.image

Keep the source image, or render a poster with the brand template when the
source has no usable media.

### Acceptance
- [ ] post.image is an uploaded asset hash, not a claim

## Review
agent: editor
depends_on: 1, 2
check: [post.caption, post.image]
rubric: rubrics/localize.md
if_not_ok: redo 1
tries: 2

## Schedule (collect, once all items are ready)
agent: publisher
uses: needs.destinations

Stage a send effect per ready post at its schedule_at; sends wait for the
digest. Then compare each receipt with the approved revision (verify); a
mismatch raises a needs-you item.
