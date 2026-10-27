---
app: ig-tools-fixture
title: IG tools fixture
version: '0.1.0'
summary: A standalone-tools app — a v2 screen that invokes the declared source capability directly, with no workflow file.
needs:
  connections: []
  capabilities:
    source: { schema: 1, capability: social.read, version: 1, action: posts, resource_kind: connection_account, effect: read }
---

# IG tools fixture

A test app for CAD-1177. It ships one `app-screens/v2` screen (`feed`)
that declares `instagram.read -> source` and invokes it through the host
tool bridge. It has NO `workflows/` — the tool is the unit of work.
