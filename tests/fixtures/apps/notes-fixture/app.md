---
app: notes-fixture
title: Notes fixture
version: '0.1.0'
summary: A third app that exists only to prove app chat is data. Its package, and nothing in the host, describes its chat.
needs:
  connections: []
---

# Notes fixture

A test app for CAD-1111. It declares its chat with `app-chat.json` (context
chips and prompts, one host capability, a text-card directive with an
`open-view` button, a directive that renders its own sandboxed screen
inline, and a subject kind) and ships one tiny screen, `post-preview`.
No host code names this app.
