---
title: "Note: {{topic}}"
goal: "One short note about the topic"
label: New note
inputs:
  topic: { ask: "Topic", example: "Weekly update" }
  writer: { ask: "Registered writer in the owner PM group" }
---

One short text note.

## Draft the note: {{topic}}
agent: {{writer}}
size: S
action: local.text.produce

Write one short plain-text note about the topic. Return it as one
text/plain artifact through the authenticated local run result envelope.

### Acceptance
- [ ] one short plain-text note
