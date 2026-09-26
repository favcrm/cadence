---
app: blog-post
title: Blog post
version: 0.1.0
summary: Get a post written, checked and published — you only step in to approve.
needs:
  connections: [publish]
---

# Working in Blog post

The CAD-494 content pilot, packaged: one post per PR in the content
repo, reviewed against `rubrics/blog.md`, published only through the
operator's release. `plan propose --workflow blog-post/blog-post`
renders the run.

## What is here

- `workflows/blog-post.md` — Brief → Draft → Images → Review → Publish.
- `rubrics/blog.md` — the reviewer's checklist and verdict format.
- `templates/` — `brief.md` and `post.md`, the shapes every post follows.

## Rules agents keep

- The reviewer is never the writer or the designer (`distinct:` keeps
  them apart at propose); the verdict binds to the PR head SHA.
- Every factual claim traces to a source in the brief — no invented
  numbers; protected terms appear verbatim.
- Each ticket's acceptance is the contract; a REVISE verdict sends the
  post back, it never reaches Publish.

## The `publish` slot

`needs.connections` declares one slot, `publish`, bound to the `local`
connection by default (`cadence app set` rebinds it — a structural
change the operator re-approves). The Publish step stages the
`publish` send on that slot: the effect lands in Needs-you as a
waiting row with the rendered post as its preview, and only the
operator's press releases it to the outbox. No step, and no agent,
ever publishes directly.
