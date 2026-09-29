# CAD-813 contract for CAD-784: verified assistant email proposals

The Campaigns page (CAD-784) shows email copy the left-chat assistant
drafted, with host-proof that the assistant authored it — and proof
of which campaign and draft revision the operator's chat turn was
about. This note is the typed seam between the CAD-813 daemon
handoff and the CAD-784 UI. It covers request minting, reads and
operator decisions only — the browser never mints provenance, and no
route here touches SMTP, connections, DB paths, or raw SQL.

## How a verified proposal comes to exist

1. The operator's left-chat message carries the shell's current App
   scope (`thread_send` with `{install_id, context_id}`); the daemon
   proves both against its store and stamps the verified binding on
   the queued message (CAD-802). The browser's scope value is never
   authority.
2. The operator mints a one-time proposal request against that chat
   message — daemon verb `app_content_proposal_request`, or
   `POST …/content/proposal-requests` with
   `{"campaign_id", "message_id", "request_id"}` over HTTP. The
   daemon validates the message exists, re-proves its server-verified
   App binding names exactly that installation and context, and
   stamps the request with the **campaign the operator named** and
   the **live content source revision** read at mint time (0 when no
   draft exists yet). Campaign and source come from the host stamp
   alone — the request body carries no token, receipt, or revision,
   and such fields refuse at the transport grammar.
3. The assigned agent's live turn redeems the request exactly once
   (daemon verb `app_content_assistant_propose`, socket-only, from
   inside its own endpoint session, naming `message` + `token` +
   `request_id` plus the draft). The daemon derives the agent from
   the connection alone and re-proves, from its own rows: the
   message addresses the caller, is `running` under exactly that
   token, the token is current under the endpoint's own scheme, the
   turn's stamped App binding names exactly that installation and
   context — and the named request is `open`, stamped for that
   message **and** the proposed campaign, with its stamped source
   still equal to the live draft revision.
4. The host validates the draft with the shared CAD-782 grammar
   (block counts/sizes, the single approved `{{first_name|Fallback}}`
   token, `https` button URLs, plain-text hygiene) and stores the
   proposal inert (`pending`) with truthful attribution
   (`actor: "assistant"`, `origin: "assistant-receipt"`) and the
   durable receipt identity. The request flips to `used` in the same
   atomic transaction.

Refused without changing any request, proposal, or draft: requests
for unknown messages/contexts or malformed campaigns; redemption
with an unknown request, a request stamped for another message, a
**wrong campaign — even in the same context** — a spent request, or
a **stale stamped source** (the draft moved after mint: re-mint and
re-review instead of attaching late output to the newer draft);
fresh proposal ids on an already-claimed message (one chat message
claims one proposal, whichever id names it first — identical bytes
do not excuse a second id); concurrent fresh-id redeems (exactly one
wins; losers meet the spent request, the per-message claim, or the
UNIQUE backstop, all mapped to the bounded refusal); browser-crafted
receipt/turn/source fields; agent calls outside the live turn;
detached children; cross-install/context scope. The identical
redemption (same id, bytes, and provenance) replays idempotently.
Agent turns can propose only: request minting, Apply, Discard,
approve, test-prepare and send-prepare stay operator-only on both
daemon RPC and HTTP.

## What CAD-784 writes: the request mint (operator-only)

- `POST /api/app-installations/{install}/contexts/{ctx}/content/proposal-requests`
  with `{"campaign_id", "message_id", "request_id"}`
  → `{"request": request}`. Identical re-mint is idempotent; a
  reused request id on different scope refuses. Agent and
  sessionless callers get 403 and store nothing.

`request` shape:

```json
{
  "request_id": "req-1",
  "install_id": "…",
  "context_id": "…",
  "campaign_id": "launch-1",
  "source_revision": 1,
  "message_id": "chat-813-1",
  "state": "open" | "used",
  "used_by": null | "prop-a1",
  "created": 1759.0,
  "decided": null | 1759.0
}
```

Suggested UI flow: after the operator's chat turn about a campaign,
mint one request per campaign the turn should feed (the turn's
message id is the `message_id`); show the stamped
`source_revision` beside the draft revision so staleness is visible
before the assistant answers.

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
    "request_id": "req-1",
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
agent, request, campaign, source revision and state. Anything with
`actor == "operator"` is operator-submitted copy (CAD-782
`app_content_propose`); never label it assistant-authored, even when
its subject matches chat text. `assistant_receipt == null` always
means "no assistant provenance". There is deliberately no HTTP route
that accepts `message`, `token`, `assistant_receipt`, `turn_id`,
`nonce` or `source_revision` on a propose body, and no HTTP route
for assistant redemption at all.

## What CAD-784 writes: Apply / Discard (existing routes, unchanged, operator-only)

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
oracle).

## Worked UI flow

1. The operator's chat turn names a campaign; the shell mints one
   request (`proposal-requests`) and keeps its `request_id` with the
   turn.
2. After the turn, `GET proposals/list` finds the `pending`
   proposal whose `assistant_receipt.request_id` matches. Its
   `campaign_id` and `source_revision` are the host's stamp, not the
   agent's claim — display them as such.
3. Show its subject/preheader/blocks beside the draft with
   Apply/Discard. The draft revision shown must equal the proposal's
   `source_revision`; otherwise the proposal is stale and Apply will
   refuse — re-render as "needs review".
4. On Apply, the new `content.revision` (source + 1) renders through
   the existing CAD-782 preview (`render` route); approval must be
   re-taken before any send preparation.

## Non-goals (owned elsewhere)

SMTP delivery and credentials (later send ticket), unsubscribe
authority (CAD-786), sender verification (CAD-785), segment/audience
scope (CAD-780), shell/chat chrome (CAD-802), visual editor widgets
(CAD-784 itself). Final-send preparation keeps refusing until
CAD-785/786 land; every render and test preparation stays labelled
`preview_only: true, send_ready: false`.
