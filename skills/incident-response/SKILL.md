---
name: incident-response
description: >
  Pragmatic, company-agnostic incident containment and postmortem closeout across
  business, design, marketing and engineering work. Use when containing a live
  issue (commercial/billing mistake, off-brand or broken asset release, campaign
  or spend misfire, or runtime/service outage) or preparing a structured
  postmortem deliverable, CAPA follow-up tickets and project guardrails. Grants
  no authority to deploy, spend, refund or send external communications without
  the workspace's required human approval.
---

# Incident response & postmortem closeout

Incidents happen across the whole company — not only in code. Use one lightweight,
two-gate workflow (`contain` then `closeout`) so urgent issues stop hurting
customers fast and leave behind clear deliverables, tracked follow-ups and
reusable guardrails instead of scattered chat notes.

This skill is a task method, not an execution grant: owner approvals for
deployments, refunds, contract commitments, external customer notices and ad
spend stay with their owning workspace rules (`skills/cadence/SKILL.md`,
`skills/company-outputs/SKILL.md`).

## Incident domains

| Domain | Typical triggers | Safe containment move |
|---|---|---|
| **Business & Commercial** | Wrong price/quote/invoice sent, SLA breach, fulfilment or partner billing error | Hold further sends/fulfilment, bound affected accounts/orders, draft corrective notice or credit note as `SAMPLE` for owner sign-off |
| **Design & Brand** | Broken layout, wrong logo/copy/legal disclaimer, inaccessible or corrupted published asset | Unpublish or roll back to the last approved asset, stage corrected asset in a clean draft for review |
| **Marketing & Communications** | Wrong audience segment, broken campaign link/UTM/form, runaway ad spend, premature announcement | Pause active send/ad set/automation where authorized (or escalate immediately with exact campaign ID), verify live landing flow, draft correction |
| **Engineering & Operations** | Production error spike, auth/data regression, failed deploy, broken integration | Verify live endpoint/telemetry, prepare smallest surgical fix or rollback in a clean worktree/branch, confirm live health post-release |

## Gate 1 — `contain` (stop impact & bound blast radius)

Do not write a postmortem while impact is still expanding, and do not guess
from local files when live state can be checked.

1. **Verify live reality first.**
   Check what customers, recipients or users actually experience right now (the
   live URL/flow, sent campaign receipt, issued document version, or runtime
   health check). Record exact timestamps and observed evidence on the incident
   ticket (`issue comment <ID>`).
2. **Apply the smallest reversible containment in a clean workspace.**
   - *Software:* fetch the integration branch and create an isolated worktree for
     a surgical fix or revert — never edit a shared checkout or bundle unrelated
     refactoring into an incident fix.
   - *Marketing / Design / Commercial:* pause or hold the affected automation,
     asset or document template within your granted scope, and prepare the
     corrected replacement as an unissued draft. Never send external apologies,
     refunds, revised quotes or live broadcasts without explicit human owner
     approval.
3. **Verify containment & log deferred blast radius.**
   Re-check the live surface after containment lands. Post a short `contain`
   receipt on the ticket naming:
   - trigger time, detection time and containment time (with timezone);
   - confirmed affected scope (customers, orders, channels, routes or assets);
   - **deferred blast-radius items** — deeper cleanup, reconciliation or
     non-urgent edge cases deliberately deferred so containment stayed fast.

## Gate 2 — `closeout` (deliverable, CAPA backlog & guardrails)

An incident is not closed when the fire goes out. Closeout turns what happened
into reviewable workspace artifacts and concrete prevention.

1. **Prepare the postmortem deliverable (internal to the workspace).**
   - Keep facts blameless and specific: executive summary, quantified customer /
     commercial / brand / operational impact, timestamped timeline, primary cause
     plus detection gap, what worked well, and corrective/preventive actions
     (CAPA). Mark any unconfirmed figure `UNRESOLVED` or `[TBC: evidence needed]`
     rather than guessing.
   - Format according to stakes and audience:
     - Always keep clean Markdown (`.md`) and, when stakeholders review a
       formatted report or print/PDF pack, render self-contained `.html` (and
       `.pdf` when requested).
     - If the workspace has an approved `company-outputs` report template or
       brand tokens, apply its CSS variables (`--doc-ink`, `--doc-accent`,
       typography); otherwise start from `references/postmortem-template.html`
       (neutral A4 print-ready CSS variables, zero scripts or external network
       loads so it previews inline inside Cadence's sandboxed viewer).
   - Attach deliverables directly to the incident ticket — **never** upload
     internal postmortems to public or third-party preview sites:
     - up to `1 MiB`: `cadence [--org <org>] issue attach <ID> <file>`
     - up to `10 MiB` (or versioned decks/PDFs):
       `cadence [--org <org>] artifact put --issue <ID> --kind <md|html|pdf|csv|image> --title "<title>" --file <file>`
2. **Materialize every open CAPA item as a tracked ticket.**
   Prose action items inside a document get forgotten. For each open corrective
   or preventive action:
   - create a focused `P1` or `P2` ticket (`cadence [--org <org>] issue new --project <key> --priority P1 "<action>" --file -`) with an explicit acceptance check and discipline/owner;
   - link it to the incident (`cadence [--org <org>] issue link <CAPA-ID> relates <INCIDENT-ID>`);
   - record the created ticket IDs in the postmortem's CAPA table before final
     attachment.
3. **Save durable guardrails to project knowledge.**
   Update `projects/<key>/index.md` (or the relevant domain SOP/checklist via
   `wiki write`) with the smallest rule, pre-flight check or runbook update that
   catches this class of failure next time. Strip secrets, customer PII and
   one-off transient state. When a new multi-step team procedure is needed,
   shape it with `skills/sop-workshop/SKILL.md`.
4. **Hand off in `status=review` (not `done`).**
   Transition the incident ticket to `status=review` (`cadence [--org <org>] issue set <ID> status=review --if-rev <rev>`, or `issue edit` locally) with a closeout comment listing:
   - attached postmortem deliverable name(s) (viewable inline on the `Evidence` tab);
   - linked CAPA ticket IDs (`P1`/`P2`) and their owners;
   - updated wiki guardrail path(s);
   - any pending human decision (customer communication sign-off, credit/refund
     approval, or final closeout acceptance).

## Boundaries

- Never claim an incident is contained or resolved without checking the live
  surface or stating why live verification is blocked.
- Never send external customer emails, social posts, refunds, invoices or
  production deployments without the workspace's required human approval.
- Never publish internal incident timelines, customer lists or postmortems to
  external preview domains; use `issue attach` or `artifact put --issue <ID>`.
