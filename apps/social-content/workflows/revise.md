---
workflow: revise
title: "Ask the agent — revise one post"
inputs:
  post:        { type: "ref<post>" }
  instruction: { type: text, optional: true }
  scope:       { type: "list<field>", default: [caption] }
mode: direct          # the person asked: write a revision, show diff + undo
---

## Apply the instruction
agent: writer               # designer when scope is image
writes: post.{scope}

Apply {{instruction}} to the scoped fields of {{post}} only. Everything else
stays untouched; the run does not satisfy a separate outstanding request.

### Acceptance
- [ ] only the scoped fields carry a new revision
- [ ] the revision is attributed "writer (asked by you)"
