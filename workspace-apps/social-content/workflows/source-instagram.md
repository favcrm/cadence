---
title: "Read public Instagram posts: {{profile_handle}}"
goal: "Retain a bounded provider receipt for an operator-selected public Instagram profile"
label: Find Instagram source
capability_slots: [source]
execution: host
inputs:
  profile_handle: { ask: "Public Instagram handle selected by the operator", example: "juicysuite_crm" }
---

Read the selected public Instagram profile through the bound `source`
capability. This is a preliminary acquisition run. It does not draft,
review, release or post content.

## Acquire Instagram source: {{profile_handle}}
size: S
action: local.capability.call

The operator selected `{{profile_handle}}` when this run was frozen, and
the host executes this step itself: it calls the frozen `source` binding
with an empty input object and retains the actual provider response. No
worker is assigned, no provider, account, company, profile, tool or URL
is added by a caller, and the broker derives the profile from the frozen
run. A refusal or an uncertain charge fails the run with the provider's
own reason; the same run is re-clicked to retry, never a second paid
request. The durable receipt is the source of truth: posts are never
invented, cached social text is never copied, and no other capability is
called.

### Acceptance
- [ ] the provider action used only the frozen `source` binding and handle
- [ ] the run retains the durable receipt and reports the honest result
- [ ] no caption, image, publication or external post is produced
