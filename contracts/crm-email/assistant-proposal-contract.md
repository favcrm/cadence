# CAD-813 contract for CAD-784: verified assistant email proposals

The Campaigns page (CAD-784) shows email copy the left-chat assistant
drafted, with host-proof that the assistant authored it. This note is
the typed seam between the CAD-813 daemon handoff and the CAD-784 UI.
It covers reads and operator decisions only — the browser never mints
provenance, and no route here touches SMTP, connections, DB paths, or
raw SQL.

## How a verified proposal comes to exist

1. The operator's left-chat message carries the shell's current App
   scope (`thread_send` with `{install_id, context_id}`); the daemon
   proves both against its store and stamps the verified binding on
   the queued message (CAD-802). The browser's scope value is never
   authority.
2. The assigned agent's live turn calls the daemon verb
   `app_content_assistant_propose` over the socket from inside its own
   endpoint session, naming `message` + `token` (its running turn),
   the install/context/campaign/proposal ids, and the draft
   (`subject`, `preheader`, `blocks`). The daemon derives the agent
   from the connection alone and re-proves, from its own rows: the
   message addresses the caller, is `running` under exactly that
   token, the token is current under the endpoint's own scheme, and
   the turn's stamped App binding names exactly that installation
   and context. `source_revision`, when named, must equal the current
   draft revision.
3. The host validates the draft with the shared CAD-782 grammar
   (block counts/sizes, the single approved `{{first_name|Fallback}}`
   token, `https` button URLs, plain-text hygiene) and stores the
   proposal inert (`pending`) with truthful attribution
   (`actor: "assistant"`, `origin: "assistant-receipt"`) and the
   durable receipt identity. Replay behind identical bytes AND
   identical provenance is idempotent; anything else on a used
   proposal id refuses.

Refused without changing any proposal or draft: browser-crafted
receipt/turn fields, agent calls outside the live turn, detached
children (`setsid`, or any process outside the endpoint session),
replay with different bytes, stale source revisions, and
cross-install/context/campaignscope. Agent turns can propose only:
Apply, Discard, approve, test-prepare and send-prepare stay
operator-only on both daemon RPC and HTTP.

## What CAD-784 reads (existing routes, unchanged)

- `GET /api/app-installations/{install}/contexts/{ctx}/content/proposals/list`
  → `{"proposals": [proposal, …]}` (100 max, ordered by proposal id).
- `GET …/content/proposals/{proposal_id}` → `{"proposal": proposal}`.

`proposal` shape (all strings server-typed, digests `sha256:…`):

```json
{
  "proposal_id": "prop-a1",
  "install_id": "…",
  "context_id": "…",
  "campaign_id": "launch-1",
  "source_revision": 1,
  "subject": "…",
  "preheader": "…",
  "blocks": [{"type": "heading"|"paragraph", "text": "…"}
            |{"type": "button", "label": "…", "url": "https://…"}],
  "content_digest": "sha256:…",
  "actor": "assistant" | "operator",
  "origin": "assistant-receipt" | "operator-direct",
  "assistant_receipt": null | {
    "message_id": "chat-813-1",
    "agent": "crm-writer",
    "install_id": "…",
    "context_id": "…",
    "campaign_id": "launch-1",
    "source_revision": 1
  },
  "state": "pending" | "applied" | "discarded",
  "created": 1759.0,
  "decided": null | 1759.0
}
```

Render rule for the Campaigns page: a proposal with
`actor == "assistant"` and a non-null `assistant_receipt` is
assistant-authored — show the verified badge with the receipt's
agent, source revision and state. Anything with
`actor == "operator"` is operator-submitted copy (CAD-782
`app_content_propose`); never label it assistant-authored, even when
its subject matches chat text. `assistant_receipt == null` always
means "no assistant provenance".

## What CAD-784 writes (existing routes, unchanged, operator-only)

- `POST …/content/proposals/{id}/apply` with `{}` or
  `{"expected_revision": N}` → `{"content": doc}`. Creates revision
  N+1 from the proposal, marks the proposal `applied`, and
  invalidates approval (`approval.valid: false`). Refuses when the
  proposal is not `pending`, when its `source_revision` drifted
  behind the current draft (stale — the operator re-reviews instead
  of merging), or when `expected_revision` names a different current
  revision.
- `POST …/content/proposals/{id}/discard` with an empty body →
  `{"proposal": proposal}` with `state: "discarded"`. Non-mutating:
  the draft digest is identical before and after.

Both require the board's operator session; agent and sessionless
callers get 403 and change nothing. Unknown proposal ids and
already-decided proposals share one refusal each (no existence
oracle). There is deliberately no HTTP route that accepts `message`,
`token`, `assistant_receipt`, `turn_id`, `nonce` or
`source_revision` on a propose body — such bodies get 400 — and no
assistant-mint path exists at all.

## Worked UI flow

1. After a chat turn, `GET proposals/list` (optionally filtered by
   campaign on daemon RPC; HTTP lists the context) and find the
   `pending` proposal whose `assistant_receipt.message_id` matches
   the turn the operator just watched.
2. Show its subject/preheader/blocks beside the draft with
   Apply/Discard. The draft revision shown must equal the proposal's
   `source_revision`; otherwise the proposal is stale and Apply will
   refuse — re-render as "needs review".
3. On Apply, the new `content.revision` (source + 1) renders through
   the existing CAD-782 preview (`render` route); approval must be
   re-taken before any send preparation.

## Non-goals (owned elsewhere)

SMTP delivery and credentials (later send ticket), unsubscribe
authority (CAD-786), sender verification (CAD-785), segment/audience
scope (CAD-780), shell/chat chrome (CAD-802), visual editor widgets
(CAD-784 itself). Final-send preparation keeps refusing until
CAD-785/786 land; every render and test preparation stays labelled
`preview_only: true, send_ready: false`.
