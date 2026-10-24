---
title: "Hello: {{topic}}"
goal: "One short greeting"
label: Hello
inputs:
  topic: { ask: "Topic", example: "World" }
  writer: { ask: "Registered writer in the owner PM group" }
---

One short greeting.

## Draft the greeting: {{topic}}
agent: {{writer}}
size: S
action: local.text.produce

Write one short plain-text greeting about the topic. Return it as one
text/plain artifact through the authenticated local run result envelope.

### Acceptance
- [ ] one short plain-text greeting
