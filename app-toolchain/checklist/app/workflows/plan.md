---
title: "Plan checklist: {{topic}}"
goal: "Propose a short checklist"
label: Plan checklist
inputs:
  topic: { ask: "What needs to be done?", example: "Prepare a team meeting" }
  writer: { ask: "Registered writer in the owner PM group" }
---

Propose a short checklist for the owner to review.

## Draft checklist: {{topic}}
agent: {{writer}}
size: S
action: local.text.produce

Return a short plain-text checklist. Do not create or change task records
without using the workspace's approved app action flow.

### Acceptance
- [ ] Checklist is relevant to the supplied topic.
