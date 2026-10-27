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

## CAD-1817 identity follow-on — candidate, not operational delivery

The follow-on adds explicit assistant-name/user-address semantics, later profile
edits, a committed identity projection and stable existing avatar selection.
Private ACK/lifecycle and workspace native/HTTP refusal checks have been exercised
in isolated fixtures, including stale avatar revisions and revoked authority.
The real compiled board was exercised at desktop/narrow widths in English and
Traditional Chinese: workspace name/personality/avatar save, later rename,
literal-name persistence and unchanged routing alias/avatar across reload and
daemon/board restart.

That workspace UI smoke is not a private-assistant provider conversation.
CAD-1603/CAD-1606 still track the isolated membership/custody/provider prerequisites;
genuine private onboarding, subsequent model replies and the full private/app
browser matrix remain unverified. The historical CAD-1420 amendment above does
not waive CAD-1817's acceptance. CAD-1818 routing/Master-query work remains blocked,
and no merge, production installation or deployment is claimed here.
