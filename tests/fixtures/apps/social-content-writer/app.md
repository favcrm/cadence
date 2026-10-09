---
app: social-content
title: Social Content
version: '0.7.0'
summary: 'Instagram drafts on platform defaults: pick a Library post, get a checked zh-HK draft, then stage Publish now for approval.'
needs:
  connections: []
  capabilities:
    source:
      schema: 1
      capability: social.read
      version: 1
      action: list_posts
      resource_kind: connection_account
      effect: read
    writer:
      schema: 1
      capability: text.generate
      version: 1
      action: generate_text
      resource_kind: connection_account
      effect: draft
    image:
      schema: 1
      capability: media.generate
      version: 1
      action: generate_image
      resource_kind: connection_account
      effect: draft
    publication:
      schema: 1
      capability: text.publish
      version: 1
      action: publish
      resource_kind: connection_account
      effect: send
---

# Social Content (writer slot fixture)

CAD-1302 fixture. The frontmatter above is copied verbatim from
cadence-app-social-content `app/app.md` (0.7.0): the text slot is named
`writer`, not the host's historic `text`. It ships one `app-screens/v2`
screen (`feed`) that invokes `caption.generate -> writer`.
