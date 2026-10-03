# Design contract: CAD-1109 app-chat/v1: one host Conversation, apps describe chat as data

Status: revised 2026-10-03 per the operator decisions on CAD-1109 (D1-D7:
Tier 2 decided; see Decisions). Docs only: no product code, schema file or
test lands with this note. It ships inside the single CAD-1108 feature PR
(CAD-1110 implements, CAD-1111's fixture app proves Tier 1 and Tier 2). Epic: CAD-1108. Companion contract:
CAD-1098 (per-app conversations; contract PR #764 head 73bcf117, UI slice PR
#771). Risk: this note changes no code. CAD-1110 adds a daemon read route, an install-time
validator and Tier 2 frame mounting, and CRM/Social bundle digests change, so
the implementation PR must be classified against
`docs/roles/risk-classes.md` and may be `human` class.

## Goals and non-goals

Problem on main (514ffc84). Two chat renderers exist: Home uses
`ThreadView.tsx`; the app shell uses `ChatPane` (`AppShell.tsx`) with
`ChatRow` (`chatRender.tsx`). The shared host hard-codes one app: the `crm`
prop, `ChatPage = customers|segments|campaigns`, fixed CRM prompts, the
built-in `ChatCsvImport`, and `installation.name === "crm"` /
`isSocial` branches. Fixes land on one side only: permission cards (CAD-1079),
ref chips and retry/discard exist on Home only. A third app would add another
branch.

Guiding principle (operator): apps work like WordPress plugins, so the core
must not limit apps. One package is installed (install is consent), extends the host
through defined hooks, and adding an app needs no core change; the core keeps
no list of apps. Unlike WP, no app code runs in the board origin, because the
board holds the operator session. The three tiers: Tier 1 data (descriptor
cards, prompts, attachments, subjects), Tier 2 the app's own sandboxed screen
inline in chat (CAD-1006), Tier 3 package-declared capabilities and actions
(direction only, Out of scope).

Goals:
- The host owns ONE Conversation component for Home and every app.
- An app describes its chat with a data-only, versioned, fail-closed
  `app-chat/v1` descriptor, mirroring `contracts/app-views/v1` and the typed
  host actions (`hostActions.ts`).
- A fixture third app works with a package alone, no core change (CAD-1111).
- Home behaviour is unchanged. Nothing CRM or Social does today is lost.

Non-goals: new message transport or send verbs; changing who may send;
data-changing actions from a card in v1 (D3); a bundled or built-in list of
apps in core (D1); app-supplied
components, markup, links, styles or code; per-user chat preferences.

Division of labour with CAD-1098. CAD-1098 decides WHICH conversation a pane
shows (server-resolved from `(installation, subject)`). This contract decides
HOW any conversation renders. They meet in two places only: the conversation
picker lives inside the shared component, and the subject kinds the picker
labels come from the descriptor.

## Data model

### The host Conversation component

One React component, `Conversation`, takes only host-derived props:

```
mode:    { kind: "home" }
       | { kind: "app", installId, contextId, screen: string|null,
           recordOpen: boolean, descriptor: AppChat | null }
density: "full" | "compact"
```

`installId`, `contextId`, `screen` and `recordOpen` come from the route and the
shell's own state (the URL), never from a message, a descriptor or a stored
draft (same rule as `hostActions.ts`). `descriptor` is the validated result of
the loader below, or `null`.

"Plain shared chat" means `mode.kind === "app"` with `descriptor === null`:
every host behaviour below, no app influence. An invalid, missing, mismatched
or unsupported descriptor yields exactly this, never a crash and never a
partially applied descriptor.

The host ALWAYS renders (no descriptor can remove, reorder or restyle these):
1. The message list: operator bubbles, assistant answers via `Md`, system
   rows, pending/failed rows.
2. The composer: send, Enter handling, read-only state, `/new` and `/clear`.
3. Permission cards (`ThreadPermission`, CAD-1079). Their approve/deny wiring
   is host-owned and operator-proven server-side; no descriptor field touches
   it.
4. Tool steps (`toolSteps`, `StepsGroup`).
5. Retry and discard on a failed send, and ref chips (CAD-574 `refs`).
6. All links: through `Md` (SafeLink with `encoded`) or `SafeLink` strict
   (CAD-1080). No other anchor exists in chat.
7. The conversation picker in app mode: General, the install's subject
   conversations, `+ New`, per CAD-1098 UI (#771): selection stored as
   `chat-conv:<install>`, collapse as `chat-collapsed:<install>`, a 404 on the
   list meaning "daemon without conversations, keep the home thread", the
   queued notice, the "Earlier history is in Home" link.
8. The context header and the waiting dot.
9. The binding stamp: sends carry the route's scope and the selected
   conversation id as a selector only (#771 / #764), no descriptor field
   reaches the request.
10. The host-fixed app-mode guards (not options): internal ids (`ctx-…`)
    never print (host constant above); and a message body that is a JSON object carrying a
    `confirm_token` or `request_id` key at any depth never prints raw: with no
    matching declared directive it shows the generic card "Confirmation sent"
    (today's `parseDirective` fallback). These apply in app mode, including
    plain shared chat. Home (`mode.kind === "home"`) keeps its current text
    rendering byte-for-byte (the #719 ThreadView tests stay green).

An app may influence only: the context chip label and up to 3 prompts per
screen or record type; which host capabilities (attachments) appear; how
each declared directive kind renders (a text card with at most 2 buttons that
run declared host actions, OR the app's own sandboxed screen mounted inline,
Tier 2); which subject kinds the picker may label; and whether the header
shows the context name (`presentation.showContext`). Nothing else.

Host constants (not descriptor fields, D4): internal ids (`ctx-...`) are
always rewritten to "this workspace" in app mode; steps are folded to the
one-line summary ("Done", "Working", "Finished with an issue" plus the step
count) in app mode and expandable (`StepsGroup`) in Home.

### The `app-chat/v1` descriptor

Same discipline as `contracts/app-views/v1`: a JSON Schema
`contracts/app-chat/v1/app-chat.schema.json` plus a TS validator
`ui/src/features/app-shell/app-chat/contract.ts` (`parseAppChat`), two
notations of one grammar, changed together. Parity is a test (CAD-1110). A
descriptor is data: JSON object, plain JSON types only, no accessors.

```
{
  "contract": "app-chat/v1",              // exactly; any other tag refuses
  "app": "crm",                           // IDENT; provenance, see Loading
  "contexts": [                           // 0..12; ids unique
    { "id": "customers",                  // IDENT: the route's screen id
      "label": "Customers",               // 1..40
      "prompts": ["Who can't be emailed?"],          // 0..3, each 1..80
      "record": { "label": "Customer",    // optional: when a record is open
                  "prompts": ["Summarise this customer"] } }  // 0..3
  ],
  "attachments": [ { "id": "csv-import", "label": "Import a customer list" } ],
                                          // 0..4; id in the host registry; label 1..40 optional
  "directives": [                         // 0..8; match unique
    { "match": "cadence_csv_import",      // ^cadence_[a-z][a-z0-9_]{0,47}$
      "card": { "title": "Customer import",              // 1..80
                "text": "You confirmed the reviewed rows. The assistant applies them.", // 0..240
                "fields": [ { "label": "Name", "from": "name" } ],   // 0..4; label 1..40; from IDENT
                "buttons": [ { "label": "Open customers", "run": "open-view", "view": "customers" } ] } }, // 0..2
    { "match": "cadence_post_preview",    // Tier 2: instead of "card"
      "render": "screen:post-preview",    // ^screen:[a-z0-9][a-z0-9-]{0,31}$ (CAD-1006 tag grammar)
      "size": "medium" }                  // optional: small|medium|large = 160|320|480 px high; default medium
  ],                                      // exactly one of card | render per directive
  "subjects": [ { "kind": "campaign", "label": "Campaign" } ],      // 0..8; kind IDENT unique; label 1..40
  "presentation": { "showContext": false }                           // optional; default false
}
```

Field notes:
- `contexts[].id` is matched to the route's `screen` (CRM: its section id;
  `recordOpen` selects `record` over the page entry). A route screen with no
  entry shows no chip and no prompts (today's Social behaviour). The
  descriptor never defines routes. The chip text is `label` (plus the host's
  fixed " (open)" suffix for the record entry, so `record.label` is
  "Customer" as today).
- Prompts only fill the composer. They never send (today's behaviour).
- `attachments[].id` must be a key of the host capability registry (below). An
  unknown id refuses the whole descriptor. `label` overrides the control label
  only.
- `directives[].card.fields[]` show a value read from the matched payload as
  TEXT, `from` naming a payload key. A card with no `fields` is OPAQUE: the
  payload is only shape-checked and never read (the CSV card, whose payload is
  `{request_id, confirm_token}`).
- `buttons[].run` is an id from the host action registry (below). `view` is
  required exactly when the action takes it (`open-view`) and must name a view
  id declared in the SAME install's app-views descriptor (loader check;
  skipped, with the button dropped, if no app-views descriptor is loaded).
- `subjects[].kind` is a display allowlist for the picker. The daemon's
  verified subject rules (#764: v1 `campaign:<id>`) remain the authority: a
  declared kind the daemon does not know produces no conversation, and a
  conversation whose kind the descriptor does not declare is listed under
  "Other" with its server title.
- `presentation.showContext` (boolean, default false, D2): the shared header
  shows "Assistant · {route context name}" only when true and the route has a
  context. CRM sets true; Social omits it and keeps today's header.
- `directives[].render` (Tier 2, D6 below): `screen:<tag>` names a screen the
  app's OWN installed bundle declares (CAD-1006). The tag grammar is the
  protocol's own `^[a-z0-9][a-z0-9-]{0,31}$`. A directive has exactly one of
  `card` or `render`; both or neither refuses. The tag is only a name: the
  mount route proves it against the approved bundle, and an unknown tag fails
  the mount (the message then renders as plain text). `size` is a host-owned
  height step; the app cannot set pixels, styles or classes.

Bounds enforced by BOTH notations unless noted: strings as above; no control
or line-separator characters (`[\u0000-\u001f\u007f  ]`) in any
string; IDENT is `^[a-z][a-z0-9_-]{0,63}$`; whole descriptor at most 16 KiB
UTF-8 serialized, at most 1024 nodes, nesting depth at most 12, all checked
BEFORE shape validation, as `scanUnsafe` does. The consumer alone enforces
what JSON Schema cannot: unique ids and `match` strings, registry membership,
`view` cross-reference, "opaque iff no fields", `additionalProperties: false`
everywhere (an unknown key refuses, it is not ignored).

Forbidden keys. The validator imports `FORBIDDEN_DESCRIPTOR_KEYS` from
`app-views/contract.ts` rather than copying it, and the schema's `propertyNames`
list is generated from, or tested equal to, that constant. The list includes
`action`, `capability`/`capabilities`, `scope`, `query`, `link`, `url`,
`href`, `install_id`, `context_id`, `token`, `path`, `file`. The grammar above
therefore avoids them as field names: buttons use `run`, attachments use `id`,
and nothing is called `action` or `capability`. If a future field truly needs
a forbidden word, it ships as `app-chat/v2`, not a v1 exception.

Fail closed. `parseAppChat` throws a typed `AppChatContractError(path,
message)` on the first violation (dotted path, as `AppViewContractError`). The
loader catches it, records one bounded diagnostic (path and message, never the
descriptor text), and the pane falls back to plain shared chat. There is no
partial acceptance: one bad prompt invalidates the whole descriptor.

All descriptor strings render as React text (`textContent`). The renderer
never uses `dangerouslySetInnerHTML`, `Md`, an anchor, an `href`, `style` or
`className` from descriptor data.

### Where it ships, how the host loads and pins it

- Ships in the app package, next to `app.md` and the app-views descriptor
  (`app-chat.json`), and is covered by the bundle digest the operator's
  install or update consents to (CAD-1119: install is consent). The install validator (CAD-811's package slice, extended in
  CAD-1110) runs the same grammar at install and update and refuses a package
  whose `app-chat.json` fails it (an invalid descriptor is caught at install
  time, and still fails closed at load time). Core keeps NO list of apps: no
  bundled descriptor map, no `installation.name` lookup anywhere (D1, I1).
- CRM and Social Content packages each gain an `app-chat.json`. Their bundle
  digests change; an operator install or update of the new package records
  consent for that digest, so no separate approval step exists. Until the
  installation runs the new package, the old digest has no descriptor, the
  route returns 404 and the pane shows plain shared chat.
- Loading. `loadAppChat(installId)` calls the one generic read route (see
  Routing and APIs), validates the returned bytes with `parseAppChat`, and
  requires `descriptor.app` to equal the installation's kind (a CRM descriptor
  served for a Social install is a mismatch, fail closed).
- Pinning. The route serves the descriptor from the bundle at the digest the
  installation consented to, never from a newer bundle on disk whose digest
  has no consent. The cache key is `(installId, descriptor_digest)` where the
  digest is the consented bundle digest (the same digest the CAD-1006 mount
  proves). The loader refetches whenever the installation's digest changes.
  A response whose digest differs from the installation's current consented
  digest is discarded: plain chat until it matches. The descriptor's own text
  never asserts a digest or revision (`digest`/`revision` are forbidden keys).
- After an app upgrade, old messages render with the NEW descriptor: a
  directive kind no longer declared falls to plain text (its JSON, with the
  guard-10 token rule still applying). Old messages never keep a descriptor
  alive.

### Directive protocol

A directive is an ordinary thread message (operator, answer or commentary
text) whose whole text, after trim, is a JSON object. No new transport, no new
daemon verb.

Shape: exactly one own key, equal to some declared `match`; its value a plain
object. Example (today's wire, unchanged):
`{"cadence_csv_import":{"request_id":"…","confirm_token":"…"}}`.

Matching, in order, all must hold, else the message renders as plain text
(the normal operator/assistant rendering, then guard 10):
1. text length at most 4096 bytes and valid JSON;
2. top-level plain object with exactly one own key, no accessors;
3. that key equals a `match` in the loaded descriptor of THIS pane's install;
4. the value is a plain object with at most 16 keys, leaves only string (at
   most 512 chars), finite number or boolean, nesting depth at most 1 (no
   nested objects or arrays);
5. if the card declares `fields`: the payload has none of the forbidden
   identity keys (`FORBIDDEN_DESCRIPTOR_KEYS`, plus `record_id`, `request_id`, `confirm_token`,
   `id`), every `from` key is present, and every referenced value is a
   string, number or boolean. A message that fails any rule is refused whole,
   not partially rendered. A card with no `fields` skips 5 and the payload is
   never read.

Rendering: a card with the descriptor's `title`, `text`, each field as
`label: value` (value truncated to 120 chars, rendered as text, and passed
through the host id-hiding constant), then the declared buttons. A directive can never
make a link: no value is parsed as markdown or URL, and the card has no anchor.

Actions. A card's buttons come ONLY from the descriptor's `card.buttons`. The
payload cannot add, rename or retarget a button; a payload key named `run`,
`view`, `action` or `buttons` is a forbidden key and refuses the message under
rule 5. A click calls the host action registry entry with parameters from the
descriptor and the route, never the payload.

Directive source. A directive from the agent is untrusted data: it can only
choose which declared card appears and fill its declared text fields. The same
shape arriving from the operator (the CSV confirm handoff) is rendered
identically; origin never widens what a directive can do.

### Tier 2: the app's own screen inline in chat (`render: "screen:<tag>"`)

A matched directive whose kind declares `render` mounts the app's own
sandboxed screen inline in the message list instead of a text card. Social post
previews and a future Open Slide deck preview need this and no core change.
It reuses the CAD-1006 protocol unchanged for isolation; read
`ui/src/features/workspace-apps/screen/screenProtocol.ts`, `screenLifecycle.ts`
and `ScreenOutlet.tsx`.

What stays exactly as CAD-1006 defines it:
- The frame is created by the host only: `<iframe sandbox="allow-scripts">`
  (no `allow-same-origin`, `allow-top-navigation`, `allow-forms`,
  `allow-popups`, `allow-modals`), `referrerPolicy="no-referrer"`, opaque
  origin (`event.origin === "null"`).
- Authority is the one-use FrameCap: the host POSTs
  `/api/app-installations/<install>/screens/<tag>/mount` (operator session, as
  today), receives `{mount,bridge_nonce,generation,tag}`, and the frame
  document is fetched with the 256-bit, 60 s, one-use cap bound to the live
  session and digest. The cap and the session cookie never enter the frame or
  any message. The bridge nonce, single init, source and generation checks are
  unchanged. `<install>` is the pane's route install, never the payload's.
- The child can send only `ready` and `state` over the port; anything else
  closes it (`parseChild`). It makes no network requests the host can see and
  no links: the sandbox gives it no navigation, and chat adds no link target.

What chat adds (CAD-1110 implements; a host-side extension in the style of
`publish-intents.v1`, the child opts in with
`{v:1, op:"ready", accepts:["chat-directive.v1"]}`):
- A chat mount is NOT pushed the CAD-1006 scope projection (installation,
  contexts, runs, outbox). It receives only the directive push
  `{v:1, op:"directive", tag, kind, data}` where `kind` is the matched `match`
  string and `data` is the validated payload. Least authority: a preview frame
  needs its payload, not the workspace.
- `data` is the payload after matcher rules 1-4: a flat plain object, at most
  16 keys, leaves string (at most 512 chars), finite number or boolean, whole
  push at most 4 KiB (far under `PUSH_BYTES_MAX`, which still binds). JSON
  text only: no functions, no binary, no nested objects, no URLs parsed.
- The identity rule applies to the frame too: rule 5's forbidden identity keys
  (`FORBIDDEN_DESCRIPTOR_KEYS` plus `record_id`, `request_id`,
  `confirm_token`, `id`) refuse a screen directive's payload to plain text, so
  no id, install, context or token ever reaches the frame through chat. Unlike
  a card, a screen directive has no opaque form: its payload is always scanned.
- A new directive for the same `(install, tag)` re-pushes to the live frame
  (push-only); the frame never reaches the host except `ready`/`state`.
- One live chat frame per `(install, tag)` mount generation; at most 3 chat
  frames mount per pane, older ones unmount to a text row ("Preview closed").
  A frame that does not send `ready` within 10 s (CAD-1006's timer) is
  replaced by the plain-text rendering of the message.
- Frame failure (mount refused, bad receipt, digest mismatch, timeout, port
  closed) falls back to plain text of the message and fires nothing else.
- v1 chat frames are text and layout only. A frame cannot fetch (no session,
  no URL, no ids), so it cannot show real images or slides. Image and asset
  previews need a host-side asset push (the host fetches an approved asset of
  that installation and pushes a bounded thumbnail), tracked in CAD-1114 under
  the same declare-and-approve rule; see Out of scope.
- The screen is part of the approved bundle: what it can do is what its
  bundle digest was approved to do, and in the board origin it can do nothing.

### Host action registry (for card buttons)

Host-owned, versioned with the host, ids only:

| id | takes | does |
|---|---|---|
| `open-view` | `view` (a declared app-views view id of this install) | navigates the shell to that view within the route's installation and context; data loads through the existing scoped, server-authorised record routes |

v1 has no mutating button (D3: `open-view` is the only v1 action). Any future id that mutates must (a) be a typed
client in the style of `hostActions.ts` with `assertCleanBody`, (b) have a
daemon route with the existing operator proof, and (c) take every identity
from the route and its parameters from the descriptor, never the payload.
Adding an id is a host PR plus a `docs/design` note; apps cannot add one.
Decision D3: v1 ships none.

### Host capability registry (attachments)

An attachment is a host-implemented composer extension, opted into by id.
The host implements each once; the descriptor only names it.

| id | host component | requires | emits |
|---|---|---|---|
| `csv-import` | `ChatCsvImport` (CAD-1016, moved unchanged out of `ChatPane`) | route scope present, operator and not read-only | operator message `{"cadence_csv_import":{request_id,confirm_token}}` via the normal send path |

Tier 3 direction (recorded, not built): which capabilities and future
data-changing actions an app may use will be declared in the package manifest
and approved with the bundle digest; see Out of scope. In v1 the registry is
host code and an attachment id outside it refuses the descriptor.

Rules: a capability is rendered only when the app mode scope is present and
the viewer is the operator; it keys its own state by `${installId}:${contextId}`
(the CAD-1016 remount rule is preserved); its server calls take scope from the
route and its own server checks remain the authority. A descriptor listing a
capability does not grant access (`capability` is a forbidden key for exactly
that reason; the descriptor key is `attachments`). Adding a capability is a
host PR with its own contract note. A capability's emitted directive still
needs a declared card to show a custom card; without one, guard 10 shows the
generic card.

### CAD-1098 compatibility

- Conversations are scoped by `(installation, subject)`; the daemon resolves
  scope. The shared component only selects, by conversation id, as #764's
  `thread_send` allows: it sends no `scope`, `subject` or `install` field of
  its own beyond the existing binding stamp.
- The picker moves from `ChatPane` into the shared component unchanged in
  behaviour: General, the install's conversations (from `conversation_list`),
  `+ New`; `/new`, `/clear` and `+ New` are the one `conversation_create`
  call and the command text is never sent; read-only viewers only select.
- Subject kinds come from `subjects`. CRM declares `campaign`; Social
  declares none (General plus New). The campaign page auto-select and the
  new-campaign assistant draft (`conversation_create(install, campaign:<id>)`)
  stay in the CRM outlet; they call a host function `openSubjectConversation(
  kind, id)` where `kind` must be a declared subject kind and `id` comes from
  the route, never from chat content.
- Home is unchanged: `mode.kind === "home"` has no picker (CAD-1098 keeps the
  board master thread), no descriptor, no chip.
- Ordering: CAD-1110 builds on the CAD-1098 UI (#771) and its backend. Nothing
  here changes a CAD-1098 wire shape.

## Routing and APIs

No new daemon RPC is defined by this contract. It reuses:
- `thread_send` with the verified binding and the conversation selector
  (CAD-1098);
- the CAD-1098 conversation list/create RPCs and board routes;
- the existing scoped record routes (`/api/app-installations/:install/
  contexts/:context/records…`) for `open-view` data;
- the app-chat descriptor read, specified below; CAD-1110 includes it.
- the CAD-1006 screen mount route, unchanged, for Tier 2.

### `GET /api/app-installations/:install/chat-descriptor` (read-only)

- Authorization: the same as the existing `/api/app-installations/*` reads:
  an operator session (board cookie plus `X-Cadence-Board`, same-origin), with
  the installation id taken from the path only and validated by the
  `segment()` grammar. An agent pane or endpoint, a detached child, a peer
  without the operator session, and a request for any other install id get the
  refusal those sibling routes give. The daemon has the matching read
  (`app_chat_descriptor(install)`), operator-proven like the sibling app reads;
  the board route calls that RPC and is at least as strict: the same proof is
  re-run on the HTTP peer, with no extra input that widens it (relay parity,
  I13). A board adversarial test asserts parity for an agent caller, a
  forged/other install id and a removed install.
- Response 200: `{"descriptor": <app-chat.json parsed>, "digest": "<consented
  bundle digest>", "app": "<installation kind>"}`, `Cache-Control: no-store`
  (the client caches by `(install, digest)`; the server never serves a
  cacheable body). The descriptor is read from the installed bundle at that
  digest, by the same confined resolver the screen mount uses (no symlinks, no
  path from the request), and is at most 16 KiB.
- 404: the installation does not exist or is removed; its bundle has
  no `app-chat.json`; the installation's consent is withdrawn (revoked) or
  stale (the bundle on disk differs from the consented digest). The body names no
  other install and no path. The client treats any 404 as "no descriptor":
  plain shared chat.
- Mismatch: if the client already holds a descriptor for `(install, digest A)`
  and the installation record now shows digest B, it discards A and refetches;
  a response whose `digest` differs from the installation's current digest, or
  whose `app` differs from its kind, is discarded (plain chat).
- Server never parses chat semantics beyond size and JSON validity; the
  grammar check is the validator's (install-time and client-side), and a
  server that served an invalid descriptor would still fail closed in the
  client.

Identity rules (all of them, restated so a reviewer can grep): `installId`,
`contextId`, record ids and conversation ids come from the route and the
server. No descriptor field and no directive payload ever supplies one.

## Migration

Every behaviour of today's chat, and where it lands. "Host" means fixed host
behaviour, no descriptor.

| Today (file) | Lands as |
|---|---|
| `ChatRow`/`ChatPane` vs `ThreadView` split (`chatRender.tsx`, `ThreadView.tsx`) | Host: one `Conversation`; `density` "compact" for the shell pane, "full" for Home. `ChatRow` and `ChatPane` deleted by CAD-1110 |
| `crm` prop; `installation.name === "crm"` for chat; `isSocial` for the chat label | Removed. Descriptor presence; no kind branches in chat code (I1) |
| `ChatPage = customers\|segments\|campaigns`; `PAGE_LABEL`/`RECORD_LABEL`/`PAGE_PROMPTS`/`RECORD_PROMPTS` (CRM) | Descriptor `contexts[]` (3 entries, `record` sub-objects), values identical |
| `chatContext(page, recordOpen)` chip "Customer (open)" | Host: chip from `contexts[id]`, `record` when `recordOpen`; " (open)" suffix is host |
| Prompt buttons fill the composer, never send | Host (unchanged) |
| Header "Assistant · {contextLabel}", null for Social | Descriptor `presentation.showContext` (CRM true, Social unset, so Social keeps today's look, D2) |
| `ChatCsvImport` inside `ChatPane` when `crm === true && binding.scope` | Descriptor `attachments:[{id:"csv-import"}]` plus host capability `csv-import` |
| `parseDirective`: `cadence_csv_import` card "Customer import" | Descriptor `directives[]` entry (opaque card, same title/text) |
| `parseDirective` fallback: body mentioning `confirm_token`\|`request_id` shows "Confirmation sent" | Host guard 10, in app mode only |
| `hideIds` rewriting `ctx-…` to "this workspace" | Host constant in app mode (D4) |
| Folded steps line "✓ Done · N steps" / "Working" / "Finished with an issue" | Host constant in app mode; Home keeps expandable `StepsGroup` (D4) |
| System rows `· {stepSummary}` | Host |
| Social chat: no chip, no prompts, no import, same pane | Social package ships a descriptor with `contexts: []`, `attachments: []`, `subjects: []` (or none: plain chat is identical). Its package digest changes; the operator's install or update records consent for it (CAD-1119) |
| CRM package descriptor delivery | CRM package gains `app-chat.json`; new digest is consented by the operator's install or update (CAD-1119); no bundled map in core |
| (new) inline previews | Tier 2 `render: "screen:<tag>"`; no CRM/Social behaviour depends on it |
| Collapse rail, waiting dot, composer disabled when read-only, empty/loading/failed states | Host |
| CAD-1098 picker, `/new`, `/clear`, queued notice, "Earlier history is in Home" (#771) | Host, in the shared component |
| Home `ThreadView` permission cards, ref chips, retry/discard | Host, now also in app panes (CAD-1079 in every app) |
| Home `ThreadView` rendering (no hide, no directive cards) | Unchanged: `mode.kind === "home"` |

Gained, not lost: app panes receive permission cards, ref chips and
retry/discard, and CRM's `ChatPane`-only `ctx-` hiding stays. Anything in this
table that CAD-1110 cannot place is a blocker, not a silent drop.

## Invariants

- I1 (no app branch, no app list): chat code (`Conversation` and everything it imports
  under `features/app-shell/chat/` and `features/home/ThreadView*`) contains no
  reference to `installation.name`, `crm`, `isSocial`, `social-content`, or any
  app kind. The loader has no map of apps either: no file in `ui/src` names an app
  kind to choose a descriptor. Test: a source scan fails on those tokens, and
  the fixture app renders, cards and screen, with its package alone and no
  code change.
- I2 (identity from the route): a directive, descriptor, prompt or attachment
  never supplies an install, context, record, conversation or subject id; sends
  carry only the route's scope and a conversation selector. Test: sends in a
  pane whose message list contains forged `install_id`/`context_id`/`record_id`
  directives carry the route scope, byte for byte.
- I3 (no cross-install directive): a directive renders as a card only when its
  `match` is declared by the descriptor of the pane's own installation, and only
  in a conversation the server resolved to that installation. A directive in
  another install's conversation or in Home never produces a card or an action
  for this pane.
- I4 (no undeclared action): the host runs a card button only when the loaded
  descriptor of the pane's install declares it, its `run` is in the host
  registry, and its parameters came from the descriptor and the route. A payload
  key cannot add or change a button. Test: spy on every network and navigation
  call.
- I5 (text only): every descriptor string (frame content is the app's own bundle in an opaque origin, never board DOM) and every payload value renders as
  React text: no HTML, markdown, anchor, `href`, style or class from descriptor
  or payload data.
- I6 (links): every anchor in chat is produced by `Md` (SafeLink `encoded`
  path) or `SafeLink` strict. Descriptor cards and directives produce none. A
  loopback href warns, never links.
- I7 (fail closed): any descriptor violation (forbidden key, a directive with both or neither of `card` and `render` at any depth,
  bound, unknown key, unknown registry id, unknown contract tag, `app`
  mismatch, digest/revision mismatch, parse error) yields plain shared chat for
  that install, all host behaviours intact; it never throws into the pane and
  never partially applies.
- I8 (pinning): the descriptor in use is the one the server reports for the
  installation's current consented package revision; a stale or differing
  revision or digest is discarded. After upgrade, a directive kind no longer
  declared renders as plain text.
- I9 (capabilities are opt-in and host-checked): an attachment appears only if
  its id is in the host registry AND the viewer is the operator, not read-only,
  and the route scope is present; listing a capability grants no server
  authority; the capability's server calls remain proven by the daemon.
- I10 (secrets never print): in app mode a message carrying `confirm_token` or
  `request_id` never renders its raw text, whatever the descriptor says (the
  generic card at worst).
- I11 (picker is host, subject kinds are labels): the picker is part of the
  shared component; subject kinds only label what the server returned; a
  descriptor cannot create a conversation, choose scope or select another
  install's conversation.
- I12 (Home unchanged): `mode.kind === "home"` renders exactly as `ThreadView`
  does on main: the #719/CAD-1029/1062 tests pass unmodified.
- I14 (frame isolation, Tier 2): a chat frame is only ever created by the host
  with `sandbox="allow-scripts"` (no same-origin, navigation, forms, popups,
  modals), `no-referrer`, from a one-use FrameCap minted for the pane's route
  install and the tag the descriptor names; the session cookie, FrameCap and
  any id never enter the frame or a message. Test: assert the exact sandbox
  token set and that the frame document request is the cap URL only.
- I15 (frame input is bounded text): the only host-to-frame data in chat is
  the `directive` push: the matched kind plus a flat, at most 16-key, at most
  4 KiB payload of string, number and boolean leaves with no forbidden or
  identity key; the CAD-1006 projection is never pushed to a chat frame.
- I16 (frame output is closed): a chat frame can send only `ready` and `state`
  (`parseChild`); any other message, a wrong source, a non-`"null"` origin, a
  second init, a wrong tag/generation/bridge nonce, or an oversized `state`
  closes the port and falls back to the text row.
- I17 (frame is this install's): the frame is mounted only for a directive the
  pane's own installation's descriptor declares, with a cap bound to that
  install, tag, session and digest; a cap is single-use (a replay fetches
  nothing); a directive from another install's conversation or Home never
  mounts a frame.
- I13 (relay parity): the descriptor read route, the screen mount route and
  every route this contract uses are at least as strict on the HTTP peer as on the daemon RPC they relay.

## Failure modes

- [ ] Crash or SIGKILL between any two steps: the contract adds no durable
  state: the descriptor is a cache keyed by `(install, digest)`, picker state
  is `localStorage` keys CAD-1098 already defines. A crash mid-load leaves
  nothing; the next mount loads again or shows plain chat (I7). Messages are
  CAD-1098's single transaction.
- [ ] Loaded host (slow step, timeout, retry, late or duplicate action): a
  slow descriptor load shows plain chat first, then the card layout when it
  validates; messages already shown re-render, no action fires on load. A late
  descriptor response for a previous install/revision is discarded by the
  `(install, digest)` check (I8). A double click on a button issues at most one
  `open-view` navigation (idempotent, no mutation) (I4).
- [ ] Wrong caller (agent pane, endpoint or detached child instead of the
  permitted actor): chat does not authorise anything; sends and capability
  calls use the existing operator-proven routes (I9, I13). An agent can emit
  a directive message but only the declared card renders (I3, I4). The descriptor
  read route requires the operator session; an agent or detached child cannot
  read or alter another install's descriptor (I13).
- [ ] Concurrent callers (same request twice, two racing): two panes
  (Home plus app) share the thread store but render independently with their
  own descriptor (I3). Two installs loading at once cannot cross: the cache key
  includes `installId` and the digest (I8). Sends race exactly as in CAD-1098.
- [ ] Forked, detached or `setsid` child (outlives the caller, inherits fd,
  token, lock): n/a for rendering, with a reason: the component runs in the
  browser and holds no fd, token or lock; a child that survives can only
  produce messages and descriptor files, both covered by the forged-field and
  malicious-package rows. The scoped turn token path is CAD-1098's I3.
- [ ] Relay paths (board or HTTP peer at least as strict as the daemon RPC):
  `GET …/chat-descriptor` takes the install id from the path only, needs the
  operator session like sibling `/api/app-installations/*` reads, returns
  nothing for another install or a revision whose consent is withdrawn, and an adversarial
  test asserts the board returns the same 404/403 as the daemon read for an
  agent caller and a forged id (I13). Conversation routes are CAD-1098's.
- [ ] Forged field (caller-supplied id, actor, head, token, path or
  timestamp). Descriptor: `install_id`, `context_id`, `token`, `url`, `action`
  or any forbidden key at any depth refuses the descriptor (I7); a forged
  `app` that differs from the installation kind refuses (I7); a forged
  `digest`/`revision` key is itself forbidden (I8). Directive: an `install_id`,
  `context_id`, `record_id`, `id`, `run`, `view`, `action` or `buttons` key in a
  field-bearing payload refuses the message to plain text (I2, I4); extra keys in
  an opaque payload are never read (I2).
- [ ] Partial write (torn file, half-applied update, event without effect):
  n/a for the component, with a reason: it writes nothing; a partially
  installed package (descriptor present, revision pending) is covered by
  "descriptor/version mismatch" below and fails closed. A torn or truncated
  descriptor is invalid JSON or fails a bound and gives plain chat (I7).
- [ ] Clock or TTL edges: no expiry exists in this contract; the descriptor
  cache has no TTL (invalidated by revision change only). The queued notice and
  the confirm token TTL are CAD-1098 / CAD-1016's. Boundary test: a descriptor
  exactly at each bound (80/240/16 KiB) accepts, one over refuses.
- [ ] Descriptor/version mismatch after an app upgrade: the installation
  moves to bundle digest B while the browser holds A's descriptor, or an
  upgraded package ships no descriptor (404, plain chat), or the
  descriptor says `app-chat/v2` or an `app` of another kind. The cache key
  and the server digest check discard A; an unknown contract tag
  refuses (I7, I8). Old messages re-render under the new descriptor; an undeclared kind is
  plain text. Rolling an app back works the same way.
- [ ] Malicious app package: it can only ship what the operator approved with
  the digest. It ships a descriptor with script-looking strings
  (rendered inert text, I5), oversized or deeply nested JSON (refused before
  shape checks, I7), forbidden keys (I7), a `run` outside the registry (I7,
  I4), a `render` tag not in its bundle (mount refused, text fallback, I17), an `attachments` id outside the registry (I7, I9), a `view` naming a
  view that does not exist (button dropped or descriptor refused, I4), a
  `match` shadowing a host directive (collides with the host's guard 10 and
  cannot reveal secrets: a field-bearing card refuses payloads with token keys and an opaque card never reads them, I10), or prompts meant to
  socially engineer ("Approve all grants"): prompts are text that only fill the
  composer, and nothing in a descriptor can approve, send, or grant (I2, I4,
  I9). It cannot reach another install's chat (descriptor is per-install, I3).
- [ ] Agent emits a directive in the conversation of another install: the
  pane renders directives only against its own install's descriptor and only
  for entries the server returned for that conversation (CAD-1098 I2). The
  same text in Home, or in a Social conversation while CRM declares the kind,
  renders as plain text and fires nothing (I3).
- [ ] Frame escape (Tier 2): the screen tries to reach the board origin,
  cookies, `window.top`, storage or the parent DOM. The opaque origin
  (`sandbox="allow-scripts"` only) denies same-origin access, the cookie is
  never sent to the frame document (cap URL only), and the host reads only
  port messages through `parseChild` (I14, I16). A frame that sends anything
  but `ready`/`state` is closed.
- [ ] Frame tries to navigate or open links (Tier 2): the sandbox has no
  top-navigation, popups or forms; chat provides no anchor for frame content;
  an attempted `location` change only reloads inside the frame and cannot leave
  the cap-bound document (I14). Frame content never becomes a board link (I6).
- [ ] Frame from another install (Tier 2): a directive for install A's kind
  seen in B's pane, or in Home, is plain text and mounts nothing; the mount
  route is called with the pane's install, and the daemon proves the tag
  against that install's approved bundle, refusing unknown tags (I17, I3).
- [ ] Cap replay (Tier 2): the FrameCap is one-use, 60 s, bound to session
  and digest; a second fetch of the same `/api/app-screen/<nonce>` is refused
  by the daemon, and a stale `generation` or bridge nonce closes the port
  (I14, I16, I17). The cap never appears in a message, log or the descriptor.
- [ ] Oversized or malformed push (Tier 2): a payload over 4 KiB, over 16 keys,
  a nested leaf, or a forbidden/identity key fails matcher rules 1-5 and the
  message renders as plain text; nothing is mounted or pushed (I15). The
  CAD-1006 `PUSH_BYTES_MAX` stays as the hard backstop.
- [ ] Unsafe links: a prompt, card title or field value containing
  `javascript:` or a URL renders as plain text (I5, I6); assistant markdown
  links still go through `Md`/SafeLink, loopback warns (I6).
- [ ] Undeclared action: a payload tries to supply or rename an action, or a
  descriptor names an unregistered `run`: refused as above (I4). Covered by the
  acceptance check.

## Acceptance check

ONE check proving the bad case is refused. It is written from this contract by
someone other than the implementer (the Spec reviewer or the ticket author);
the implementer may not edit it. It lives in `ui/tests/appChatDirective.test.tsx`.

Setup: mount the shared `Conversation` in app mode with a descriptor declaring
exactly one directive `cadence_note` (field `title`), with one button
`{label:"Open customers", run:"open-view", view:"customers"}`. Spy on `fetch`
and on the shell navigation callback.

Case: the message list contains (a) a valid `{"cadence_note":{"title":"Hi"}}`,
which must render a card with the title, the text "Hi" and exactly one button
(proving the guard is not vacuous), and (b) the forged
`{"cadence_note":{"title":"Hi","action":"open-view","view":"campaigns"}}`,
(c) a descriptor naming `run:"delete-everything"`, and (d) a second
declared kind with `render:"screen:post-preview"` receiving
`{"cadence_post_preview":{"text":"Hi","install_id":"other"}}`.

| Check | Proves (I#) | Guard | Wrong outcome without the guard |
|---|---|---|---|
| `directive_naming_an_undeclared_action_renders_text_and_issues_no_host_request` | I4 (also I2, I7) | forbidden-key refusal in the directive matcher (`matchDirective` rule 5), `run` registry check in `parseAppChat`, and the button list built only from the descriptor | (b) renders a card with a second or retargeted button, or clicking runs `open-view` for `campaigns`, so `fetch`/navigation is called; (c) is accepted and the button calls an unknown id; (d) mounts a frame (a mount request is issued) with a forged install id in its push |

Pass requires: (b) renders as plain message text (the JSON as text), shows no
button, and clicking anything issues zero `fetch` calls and zero navigation
calls except the one declared button in (a) navigating to `customers`; (c)
yields plain shared chat with the composer intact; (d) renders as plain text
and issues no `/screens/…/mount` request (spy shows zero mount fetches).

One check suffices for Tier 2 because its chat-specific bad case (an identity or
action key reaching the frame) fails at the same matcher rule 5 the card case
uses, so (b) and (d) exercise one guard. The frame's own isolation (sandbox,
one-use FrameCap, push-only, closed child messages) is CAD-1006's guard with its
existing adversarial tests, which CAD-1110 must keep green; CAD-1110 adds
tests for I14-I17 as supporting tests, not a second acceptance check.

Supporting tests CAD-1110 adds, each failing without its guard (not the
acceptance check, listed so the implementation is not trusted on one test):
a source scan for I1; descriptor schema/validator parity; forbidden-key list
equality with `app-views`; unknown `app`/tag/bound refusals; a cross-install
directive; a descriptor swap after revision change; token-bearing message
never printed; Home snapshot unchanged (#719 tests).

## Decisions

All decided by the operator on 2026-10-03 (CAD-1109 comment, relayed by
cc13-pm). No open decisions remain.

- D1 Descriptor source: DECIDED. It ships in the package (`app-chat.json` next
  to `app.md`), covered by the consented bundle digest, served per installation
  through one generic operator-session-gated read-only route pinned to the
  consented digest (specified in Routing and APIs). The bundled loader map is
  rejected: core keeps no list of apps. CRM and Social packages gain
  descriptors; their new digests are consented by the operator's install or update (CAD-1119). CAD-1110 includes
  the route.
- D2 Header: DECIDED. `presentation.showContext` (boolean, default false);
  Social keeps today's look.
- D3 Card actions: DECIDED. `open-view` is the only v1 button action; no
  data-changing actions from chat cards in v1.
- D4 Presentation constants: DECIDED. Hide-internal-ids and fold-steps are host
  constants, not descriptor fields.
- D5 Screen ids: DECIDED. `contexts[].id` is matched to the shell's own section
  id (CRM's section ids today), never to app-views view ids.
- D6 Tier 2 in v1: DECIDED. A directive may declare `render: "screen:<tag>"`
  and the host mounts the app's own sandboxed screen inline via CAD-1006.
- D8 `chat-directive.v1`: DECIDED (operator, 2026-10-03). The opt-in directive
  push for chat frames is adopted exactly as specified in Tier 2.
- D7 Tier 3 direction: DECIDED as direction only (Out of scope below).

## Use cases

Each row is delivered by that app's package alone (descriptor plus, for Tier 2,
its own screen); no core change for any of them.

| App | What its package declares | Core change |
|---|---|---|
| CRM | contexts for Customers/Segments/Campaigns with record prompts, `csv-import` attachment, `cadence_csv_import` opaque card, `subjects:[campaign]`, `showContext: true` | none beyond the one-time move out of `ChatPane` |
| Social Content | an empty or minimal descriptor (today's look), or `cadence_post_preview` with `render: "screen:post-preview"` for inline post previews | none |
| Blog Post | contexts per screen (Drafts, Published) with prompts, a card for "draft ready" with `open-view` to its drafts view, optional `screen:draft-preview` | none |
| Open Slide deck app (future) | a `cadence_deck_preview` directive with `render: "screen:deck-preview"`, `size: "large"`, prompts per screen | none |
| Agency running two clients | one installation with two contexts; the shared header names the client (`showContext: true`); context comes from the route so a directive can never address the other client; conversations are per `(installation, subject)` | none |

## Out of scope

- Implementation of the Conversation component, the validator and schema files,
  the registries, the descriptor route, Tier 2 chat mounting and tests:
  CAD-1110.
- The fixture third app and its package (Tier 1 and Tier 2): CAD-1111.
- Per-app conversations backend, subject verification, conversation RPCs,
  scoped powers, session switching: CAD-1098 (contract #764; UI #771).
- Package install/approval pipeline generally (CAD-811). The `app-chat.json`
  validator-at-install and the `chat-descriptor` daemon/board read are in
  CAD-1110 (D1).
- v1 chat frames are text and layout only (a frame cannot fetch). Real image,
  asset and slide previews need a host-side asset push of a bounded thumbnail
  of an approved asset of the installation: CAD-1114, under the same
  declare-and-approve rule as Tier 3.
- Tier 3 (direction, not built, forward reference: CAD-1114): capabilities (attachments) and any future data-changing
  card actions will be declared in the package manifest and approved by the
  operator with the bundle digest, replacing the host-code-only registries; a
  capability the package does not declare stays unavailable. v1 keeps the host
  registries (`csv-import`, `open-view`) and no declared mutation.
- Any mutating card action, new attachments beyond `csv-import`, richer card
  layouts, descriptor-defined slash commands, localisation of descriptor
  strings: not scheduled; each needs its own contract note.
- Changes to Home beyond adopting the shared component (CAD-1029/1062 own
  ThreadView); notes and standing-approval policy (CAD-411, CAD-814).
