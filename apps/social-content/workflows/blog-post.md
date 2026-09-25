---
workflow: blog-post
title: "Write a blog post"
inputs:
  brief: { type: text }
creates: post             # kind: article — outside the drafting standing approval
mode: planned             # opens a plan card first; the person approves scope
---

## Brief
agent: strategist
writes: post.brief

Turn {{brief}} into an outline the client would recognise: angle, sections,
the protected terms that must appear, and the calls to action.

### Acceptance
- [ ] outline names every protected term it will use
- [ ] one CTA, tied to the client's current campaign

## Draft
agent: writer
depends_on: 1
writes: post.body

Write the article in the client's voice. zh-HK by default for Kura Ramen;
en for Velvet Padel. Facts come only from the brief — never from the
untrusted feed text.

### Acceptance
- [ ] body keeps every protected term verbatim
- [ ] no claim appears that the brief does not support

## Images
agent: designer
depends_on: 1
writes: post.image

Hero image plus section art, brand template.

## Review
agent: editor
depends_on: 2, 3
check: [post.body, post.image]
rubric: rubrics/localize.md
if_not_ok: redo 2
tries: 2

Publishing a blog post is an outward act: after review it waits in
Needs-you like any other send.
