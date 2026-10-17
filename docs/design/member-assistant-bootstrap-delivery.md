# Member assistant bootstrap — delivery scope (CAD-1420, PR #1013)

## Amended acceptance scope
The operator approved a bounded scope amendment: after an explicit question whether to
accept the native implementation on green CI plus guard/lifecycle code evidence while
deferring the live-reply check as unverified, the operator replied "be pragmatic, ship
fast". This is a recorded scope amendment, not a claim that an unrun check passed.

## What this native PR delivers
- Optional, request-first BOOTSTRAP.md: the member's real request is answered first;
  personalization asks at most one optional question per turn and accepts skip/later.
- One-time seed of BOOTSTRAP.md in the canonical private principal workspace without
  overwriting edits; agreed name/tone/preferences persist to IDENTITY.md, SOUL.md and
  USER.md; platform AGENTS.md is not rewritten by chat.
- Authoritative onboarding state (pending/complete/skipped/deferred) bound to the
  canonical organization/principal/assistant, surviving restart and credential rotation.
- Removal of only the HOSTED_CADENCE_ASSISTANT_ENABLED availability gate and its HTTP
  counterpart; missing custody/provider readiness stays a truthful refusal, never an
  unguarded fallback.

## Acceptance evidence for native code delivery
- Required exact-head CI and independent guard/lifecycle code review; Markdown is not
  authority — prompt text and paths cannot grant tools, approval or foreign access.
- First six functional/security acceptance items are unchanged: caller-bound
  authorization, original-CID, live membership, replay protection, service custody with
  explicit consent, private-cwd confinement, manual stop, canonical jobs preserved.

## Explicitly deferred — UNVERIFIED
- Genuine authorized real-provider first request/reply (R1) and hosted end-to-end
  first-chat acceptance remain unverified and are deferred to separately tracked
  operational acceptance under AOS-233, related to AOS-216 custody/revalidation.
- No provider, login, issuer or infrastructure setup is required to land this native
  code PR, and none is claimed.

## Unchanged gates
Independent review, operator approval for human-class triggers, merge-queue flow and
all production-safety rules apply as normal. No release, deployment or activation is
authorized or claimed by this document.
