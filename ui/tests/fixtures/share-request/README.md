# Share-request fixtures (AOS-711)

Synthetic, shared payloads for "a member asks an owner to share an owner-only
report". The Cadence board renders and calls against these files;
`packages/contracts/src/share-request.test.ts` asserts every file parses.

Schemas: `packages/contracts/src/share-request.ts`. Names, numbers and ids are made up.

## What a share request is

A member's Nova offers "ask an owner" after `canvas_open` refused `owner_only`;
if the member agrees it calls the `share_request` tool `{app, view, params?, reason?}`.
The request is the app, the view and its params, exactly, and the reason. No
access changes. If an owner shares it, the host renders that view as the owner
into a read-only PDF, stores it as a private file of the member (nobody else in
the workspace can open it), and the member's thread shows it as an ordinary
`artifact` card. The viewer and download are the existing file viewer.

Asking again is always allowed. Only a request still pending (or being shared)
for the same member, app, view and params is refused as a duplicate; the member
then sees their existing card.

## Three shapes, one request id (`sr_` + 32 hex)

1. **Member card**, `share_request` chat-card entry in the member's thread
   (`card-*.json`). `payload.status` is `pending`, `shared`, `declined` or
   `cancelled`; the thread is rewritten in place when it changes, so a card
   keeps its position. `reason` is the member's words, `note` the owner's,
   both shown as typed, plain text. A blank decline note is absent: draw
   `"<decided_by> declined."`. When `shared`, `file` names the file and the
   thread has the `artifact` entry right after the card
   (`artifact-card-after-shared.json`): draw "Shared by `decided_by`" on the
   card and open the file in the canvas from the artifact card.
2. **Owner To do item**, a staged action in EVERY owner's Home queue,
   `GET /api/assist/actions` on the owner's Home conversation (`owner-item-*.json`;
   `tool: "share_request"`, `input` is `shareRequestActionInputSchema`).
   `input.params` is a URL query, `name=value&...`. The row follows the one
   request: `pending` while open; `approved` + `result.code "shared"` once any
   owner shared it; `denied` once declined (`decisionReason` = the note) or
   cancelled. So the first owner to decide settles it for all the others.
   **Decide only through the share-request routes.** Using the generic
   `/api/assist/actions/:id/approve` leaves the row pending (`retry`), and
   `/deny` answers 404, because neither can see the shared request.
3. **Decisions**, board session, same origin:
   - `GET  /api/share-requests/:id` the request (`{ok, request: <card payload>}`), for its member or any owner.
   - `POST /api/share-requests/:id/share` owner only, no body. Renders and stores the PDF as the owner at this moment, then answers `decision-shared.json`. A render failure answers `503 snapshot_unavailable` and the request stays pending for everyone. The snapshot is frozen at this moment and never updates.
   - `POST /api/share-requests/:id/decline` owner only, body `decline-body.json` (`note` optional, at most 500 chars, blank is none). Answers `decision-declined.json`.
   - `POST /api/share-requests/:id/cancel` the requesting member only, no body, pending only. Answers `{ok, id, status: "cancelled"}`.
   - First decision wins. A later one answers `409` with `refusal-already-decided.json`: `status` and `decided_by` name the winner, so a late drawer can say "Chris already shared this". A member cancelling after an owner decided gets the same 409. Any other member's id, or an unknown id, is `404 not_found`. A non-owner on share/decline is `403 owner_required`.
   - Repeating the SAME decision by the SAME person is a safe retry (it finishes the notices and answers `200`).

## Print route the board must provide

The PDF is made by a headless browser opened as the deciding owner on the
board's own host. The board PR provides the page it loads:

`GET /print/view?install=<install_id>&screen=<screen_tag>&section=<section>[&view=<view_id>][&record=<record_id>][&params=<url-encoded name=value&...>]`

It draws that one view read-only, with the owner's own authority, and sets
`window.__printReady = true` once drawn. Until it exists a share answers
`503 snapshot_unavailable` and changes nothing.
