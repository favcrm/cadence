# Design contract: CAD-1109 app-chat/v1: one host Conversation, apps describe chat as data

Status: draft for Standards and independent Spec/security review. Docs only:
no product code, schema file or test lands in this PR. CAD-1110 implements,
CAD-1111 proves it with a fixture app. Epic: CAD-1108. Companion contract:
CAD-1098 (per-app conversations; contract PR #764 head 73bcf117, UI slice PR
#771). Human trigger: none of the `docs/roles/risk-classes.md` code paths
change here; the implementation PRs re-check this (message delivery and
scoped turns stay CAD-1098's).

## Goals and non-goals

Problem on main (514ffc84). Two chat renderers exist: Home uses
`ThreadView.tsx`; the app shell uses `ChatPane` (`AppShell.tsx`) with
`ChatRow` (`chatRender.tsx`). The shared host hard-codes one app: the `crm`
prop, `ChatPage = customers|segments|campaigns`, fixed CRM prompts, the
built-in `ChatCsvImport`, and `installation.name === "crm"` /
`isSocial` branches. Fixes land on one side only: permission cards (CAD-1079),
ref chips and retry/discard exist on Home only. A third app would add another
branch.

Goals:
- The host owns ONE Conversation component for Home and every app.
- An app describes its chat with a data-only, versioned, fail-closed
  `app-chat/v1` descriptor, mirroring `contracts/app-views/v1` and the typed
  host actions (`hostActions.ts`).
- A fixture third app works with data only (CAD-1111).
- Home behaviour is unchanged. Nothing CRM or Social does today is lost.

Non-goals: new daemon verbs or message transport; changing who may send;
mutating host actions from a card (see Open decisions D3); app-supplied
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
    never print; and a message body that is a JSON object carrying a
    `confirm_token` or `request_id` key at any depth never prints raw: with no
    matching declared directive it shows the generic card "Confirmation sent"
    (today's `parseDirective` fallback). These apply in app mode, including
    plain shared chat. Home (`mode.kind === "home"`) keeps its current text
    rendering byte-for-byte (the #719 ThreadView tests stay green).

An app may influence only: the context chip label and up to 3 prompts per
screen or record type; which host capabilities (attachments) appear; how
each declared directive kind renders as a card, and which declared host
actions its (at most 2) buttons run; which subject kinds the picker may label;
and two presentation options. Nothing else.

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
                "buttons": [ { "label": "Open customers", "run": "open-view", "view": "customers" } ] } } // 0..2
  ],
  "subjects": [ { "kind": "campaign", "label": "Campaign" } ],      // 0..8; kind IDENT unique; label 1..40
  "presentation": { "steps": "summary", "hideInternalIds": true }   // optional
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
- `presentation.steps`: `"summary"` (one line "✓ Done · N steps", today's
  app pane) or `"expandable"` (Home's `StepsGroup`). Default `"summary"` in
  app mode. `hideInternalIds` default `true`; setting `false` only stops
  rewriting `ctx-…`; it never relaxes guard 10's token rule.

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

- Ships in the app package next to the app-views descriptor (file name
  `app-chat.json` beside whatever CAD-811 names the app-views file). The
  install validator (CAD-811) runs the same grammar; today `app.md` front
  matter refuses `ui`/`schema`/`actions`, so package-shipped descriptors are
  CAD-811 work. Until then CAD-1110 loads descriptors from a bundled map
  `app-chat/bundled.ts` keyed by installation kind (`crm`,
  `social-content`, plus the fixture), living only in the loader module. The
  chat component never sees `installation.name`. See Open decision D1.
- Loading. `loadAppChat(installId)` returns `AppChat | null`. Source of truth
  is the installation record: the daemon returns, for the route's install id
  only, `{descriptor, package_revision, descriptor_digest}` for the CURRENT
  approved revision. The client validates the bytes with `parseAppChat`,
  and requires `descriptor.app` to equal the installation's kind (a CRM
  descriptor served for a Social install is a mismatch, fail closed).
- Pinning. The cache key is `(installId, descriptor_digest)`. The digest and
  package revision are server-computed at install/update approval; the
  descriptor's own text never asserts either (`digest`/`revision` are forbidden
  keys). The loader refetches whenever the installation's `package_revision`
  changes (the shell already refreshes the installation). A response whose
  revision or digest differs from the installation record's current values is
  discarded: plain chat until it matches. A 404 or older daemon means plain
  chat.
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
through `hideInternalIds`), then the declared buttons. A directive can never
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

### Host action registry (for card buttons)

Host-owned, versioned with the host, ids only:

| id | takes | does |
|---|---|---|
| `open-view` | `view` (a declared app-views view id of this install) | navigates the shell to that view within the route's installation and context; data loads through the existing scoped, server-authorised record routes |

v1 has no mutating button. Any future id that mutates must (a) be a typed
client in the style of `hostActions.ts` with `assertCleanBody`, (b) have a
daemon route with the existing operator proof, and (c) take every identity
from the route and its parameters from the descriptor, never the payload.
Adding an id is a host PR plus a `docs/design` note; apps cannot add one.
Open decision D3 covers whether v1 should ship a mutating action at all.

### Host capability registry (attachments)

An attachment is a host-implemented composer extension, opted into by id.
The host implements each once; the descriptor only names it.

| id | host component | requires | emits |
|---|---|---|---|
| `csv-import` | `ChatCsvImport` (CAD-1016, moved unchanged out of `ChatPane`) | route scope present, operator and not read-only | operator message `{"cadence_csv_import":{request_id,confirm_token}}` via the normal send path |

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
- one read for the descriptor: `GET /api/app-installations/:install/
  chat-descriptor` returning `{descriptor, package_revision,
  descriptor_digest}` for the CURRENT approved revision of that install, or
  404. It is implemented in CAD-811's package slice; until then the bundled
  loader stands in and nothing calls the route. The route is read-only, takes
  the install id from the path only, is operator-session proven like the other
  `/api/app-installations/*` reads, and is at least as strict as the daemon
  read it relays (same proof on the HTTP peer). It returns no descriptor for a
  removed, unapproved or other-install id.

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
| Header "Assistant · {contextLabel}", null for Social | Host from route context name; see Open decision D2 for Social |
| `ChatCsvImport` inside `ChatPane` when `crm === true && binding.scope` | Descriptor `attachments:[{id:"csv-import"}]` plus host capability `csv-import` |
| `parseDirective`: `cadence_csv_import` card "Customer import" | Descriptor `directives[]` entry (opaque card, same title/text) |
| `parseDirective` fallback: body mentioning `confirm_token`\|`request_id` shows "Confirmation sent" | Host guard 10, in app mode only |
| `hideIds` rewriting `ctx-…` to "this workspace" | Host in app mode; descriptor `presentation.hideInternalIds` (default true) |
| Folded steps line "✓ Done · N steps" / "Working" / "Finished with an issue" | `presentation.steps: "summary"` (default in app mode); `"expandable"` gives Home's `StepsGroup` |
| System rows `· {stepSummary}` | Host |
| Social chat: no chip, no prompts, no import, same pane | Descriptor with `contexts: []`, `attachments: []` (or no descriptor at all) |
| Collapse rail, waiting dot, composer disabled when read-only, empty/loading/failed states | Host |
| CAD-1098 picker, `/new`, `/clear`, queued notice, "Earlier history is in Home" (#771) | Host, in the shared component |
| Home `ThreadView` permission cards, ref chips, retry/discard | Host, now also in app panes (CAD-1079 in every app) |
| Home `ThreadView` rendering (no hide, no directive cards) | Unchanged: `mode.kind === "home"` |

Gained, not lost: app panes receive permission cards, ref chips and
retry/discard, and CRM's `ChatPane`-only `ctx-` hiding stays. Anything in this
table that CAD-1110 cannot place is a blocker, not a silent drop.

## Invariants

- I1 (no app branch): chat code (`Conversation` and everything it imports
  under `features/app-shell/chat/` and `features/home/ThreadView*`) contains no
  reference to `installation.name`, `crm`, `isSocial`, `social-content`, or any
  app kind. Test: a source scan fails on those tokens, and the fixture app
  renders with its descriptor and no code change.
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
- I5 (text only): every descriptor string and every payload value renders as
  React text: no HTML, markdown, anchor, `href`, style or class from descriptor
  or payload data.
- I6 (links): every anchor in chat is produced by `Md` (SafeLink `encoded`
  path) or `SafeLink` strict. Descriptor cards and directives produce none. A
  loopback href warns, never links.
- I7 (fail closed): any descriptor violation (forbidden key at any depth,
  bound, unknown key, unknown registry id, unknown contract tag, `app`
  mismatch, digest/revision mismatch, parse error) yields plain shared chat for
  that install, all host behaviours intact; it never throws into the pane and
  never partially applies.
- I8 (pinning): the descriptor in use is the one the server reports for the
  installation's current approved package revision; a stale or differing
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
- I13 (relay parity): the descriptor read route and every route this contract
  uses are at least as strict on the HTTP peer as on the daemon RPC they relay.

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
  nothing for another install or an unapproved revision, and an adversarial
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
  moves to revision N+1 while the browser holds N's descriptor, or the
  descriptor says `app-chat/v2` or an `app` of another kind. The cache key
  and the server revision/digest check discard N; an unknown contract tag
  refuses (I7, I8). Old messages re-render under N+1; an undeclared kind is
  plain text. Rolling an app back works the same way.
- [ ] Malicious app package: ships a descriptor with script-looking strings
  (rendered inert text, I5), oversized or deeply nested JSON (refused before
  shape checks, I7), forbidden keys (I7), a `run` outside the registry (I7,
  I4), an `attachments` id outside the registry (I7, I9), a `view` naming a
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
`{"cadence_note":{"title":"Hi","action":"open-view","view":"campaigns"}}`
and (c) a descriptor naming `run:"delete-everything"`.

| Check | Proves (I#) | Guard | Wrong outcome without the guard |
|---|---|---|---|
| `directive_naming_an_undeclared_action_renders_text_and_issues_no_host_request` | I4 (also I2, I7) | forbidden-key refusal in the directive matcher (`matchDirective` rule 5), `run` registry check in `parseAppChat`, and the button list built only from the descriptor | (b) renders a card with a second or retargeted button, or clicking runs `open-view` for `campaigns`, so `fetch`/navigation is called; (c) is accepted and the button calls an unknown id |

Pass requires: (b) renders as plain message text (the JSON as text), shows no
button, and clicking anything issues zero `fetch` calls and zero navigation
calls except the one declared button in (a) navigating to `customers`; (c)
yields plain shared chat with the composer intact.

Supporting tests CAD-1110 adds, each failing without its guard (not the
acceptance check, listed so the implementation is not trusted on one test):
a source scan for I1; descriptor schema/validator parity; forbidden-key list
equality with `app-views`; unknown `app`/tag/bound refusals; a cross-install
directive; a descriptor swap after revision change; token-bearing message
never printed; Home snapshot unchanged (#719 tests).

## Open decisions

- D1 Descriptor source before packages carry it. Recommend: CAD-1110 ships a
  bundled map `app-chat/bundled.ts` (loader module only) for `crm`,
  `social-content` and the fixture, replaced by the `chat-descriptor` route when
  CAD-811 lands. Alternative: block CAD-1110 on CAD-811 (cleaner provenance,
  delays the epic).
- D2 Social header. Today `isSocial` hides the context label. Recommend: the
  shared header shows the route's context name whenever the route has one, so
  Social shows its brand (one visible, intended change). Alternative: a
  `presentation.showContext` boolean, which spends a grammar field on one app.
- D3 Mutating card actions. Recommend: v1 registry has only `open-view` (no
  mutation, nothing for an agent to abuse). Alternative: add a typed
  `record-update` button now (needs a daemon route and a `docs/design` note per
  action); defer to a v1.1 once a real app needs it.
- D4 Presentation options. The epic lists "hide ids, fold steps". Recommend
  dropping both descriptor fields and making them host constants (hide ids
  always, steps summary in app mode, expandable in Home), because no app needs
  the other value; the grammar then has no `presentation`. This contract keeps
  them to match the epic text; deleting them is a backwards-compatible
  simplification before CAD-1110 starts.
- D5 Where a screen id comes from. Recommend the shell exposes `screen` as an
  IDENT it already holds (CRM section) and `contexts[].id` matches it.
  Alternative: key by app-views view ids, which would force CRM's Segments and
  Campaigns into app-views before chat can describe them.

## Out of scope

- Implementation of the Conversation component, the validator and schema files,
  the registries, the bundled loader and tests: CAD-1110.
- The fixture third app and its descriptor: CAD-1111.
- Per-app conversations backend, subject verification, conversation RPCs,
  scoped powers, session switching: CAD-1098 (contract #764; UI #771).
- Package-shipped descriptors, the install validator accepting
  `app-chat.json`, the `chat-descriptor` daemon/board read: CAD-811.
- Any mutating card action, new attachments beyond `csv-import`, richer card
  layouts, descriptor-defined slash commands, localisation of descriptor
  strings: not scheduled; each needs its own contract note.
- Changes to Home beyond adopting the shared component (CAD-1029/1062 own
  ThreadView); notes and standing-approval policy (CAD-411, CAD-814).
