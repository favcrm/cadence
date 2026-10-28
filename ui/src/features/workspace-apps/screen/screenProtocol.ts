/**
 * CAD-1006 screen bridge protocol — the closed typed v1 wire between the
 * trusted host (`ScreenOutlet`) and an opaque sandboxed app frame.
 *
 * Two nonce concepts are kept strictly distinct:
 *
 *  - **FrameCap** — the 256-bit bearer in the frame's URL
 *    (`/api/app-screen/<nonce>`), minted operator-only by the daemon,
 *    one-use, 60 s TTL, bound to the live session + digest. It *is* the
 *    authority for the document fetch. It is never sent to the child and
 *    never appears in any message.
 *  - **Bridge nonce** — a separate non-authorizing random 64-hex token the
 *    host stamps into the served document's bootstrap JSON. The child
 *    echoes it in its init to prove *which mount* the port belongs to. It
 *    grants nothing by itself; single-init + source + generation are the
 *    real guards.
 *
 * The child is opaque-origin (`sandbox="allow-scripts"`): `event.origin`
 * on its init is the literal string `"null"`. The child posts its init to
 * the EXACT parent origin (`boot.parent_origin`) — never `"*"`.
 */

/** The bootstrap JSON the host stamps into the served document, read by
 *  the child from the `application/json` block `id="cadence-screen-boot"`.
 *  `generation` is a non-negative safe-integer JSON number — the host's
 *  current mount identity for this `(install_id, tag)`, bound to the cap
 *  at mint time. `parent_origin` is the server-derived exact board origin
 *  the child must postMessage to. */
export interface ScreenBoot {
  tag: string;
  bridge_nonce: string;
  generation: number;
  parent_origin: string;
}

/** The child's one init envelope — a window `message` event whose ports[0]
 *  is the fresh `MessagePort`. Field names are exact; the host matches
 *  `tag`, `bridge_nonce` and `generation` against the live mount. */
export interface ScreenInit {
  v: 1;
  op: "init";
  tag: string;
  bridge_nonce: string;
  generation: number;
}

/** The only messages the child may ever send over the port. Anything
 *  else closes the port. `state` is the child's opaque draft snapshot
 *  (≤ 32 KiB string), held host-side in memory only; `asset` is screen.v2. */
export type ChildToHost =
  | { v: 1; op: "ready"; accepts?: Accept[] | [typeof CHAT_DIRECTIVE_V1] }
  | { v: 1; op: "state"; data: string }
  | { v: 2; op: "asset"; ref: string }
  | CallRequest
  | SlotRequest
  | ToolRequest;

/** CAD-1615 — the host tells the child which surface its frame is in.
 *  `drawer` means the shell presents the frame in the board-wide drawer
 *  (the whole window height, the board under a scrim); the child draws
 *  its surface full-bleed there. `outlet` is the normal app pane. The
 *  host owns the geometry; the surface is the app's. */
export type ScreenPresentationMode = "outlet" | "drawer";
export type ScreenPresentation = { v: 1; op: "presentation"; mode: ScreenPresentationMode };

/** CAD-1777 — the host-owned surface a frame is mounted as. A `primary`
 *  frame stays in its normal layout box; a `drawer` frame lives in the
 *  shell's own fixed overlay. The role is derived by the host from which
 *  mount it performed (a private parent token), never claimed by the
 *  frame: a ready opt-in and call arguments cannot choose it. `modal` is
 *  the frame's own overlay state — a primary is modal while its child
 *  drawer is open; a drawer hosts no further overlay. */
export type ScreenSurfaceRole = "primary" | "drawer";
export type ScreenSurface = { v: 1; op: "surface"; role: ScreenSurfaceRole; modal: boolean };

/** CAD-1777 — the two host-local presentation verbs. They are UI
 *  capabilities of the shell (which surface this frame's geometry shows
 *  in), never records operations or authority grants: a records frame is
 *  admitted to them only when it negotiated `screen-presentation.v1`, the
 *  live shell lends a drawer callback, and the frame has an action host. */
export const SCREEN_DRAWER_OPEN = "screen.drawer.open" as const;
export const SCREEN_DRAWER_CLOSE = "screen.drawer.close" as const;
export const SCREEN_DRAWER_VERBS = [SCREEN_DRAWER_OPEN, SCREEN_DRAWER_CLOSE] as const;
export type ScreenDrawerVerb = (typeof SCREEN_DRAWER_VERBS)[number];
export const isScreenDrawerVerb = (verb: string): verb is ScreenDrawerVerb =>
  (SCREEN_DRAWER_VERBS as readonly string[]).includes(verb);
/** The closed surface idiom a drawer request may name. */
export const DRAWER_SURFACE = /^[a-z0-9_-]{1,64}$/;
/** What a validated drawer request asks the shell to do. */
export type ScreenDrawerRequest = { mode: "drawer"; surface: string } | { mode: "outlet" };
/** Parse one drawer request's EXACT arguments, or `null` when they are not
 *  the closed shape (`{surface}` for open, `{}` for close). The host's
 *  argument guard and the records dispatch share this one parser, so a
 *  malformed request can never change geometry on either path. */
export function parseScreenDrawerCall(verb: ScreenDrawerVerb, args: ActionArgs): ScreenDrawerRequest | null {
  if (verb === SCREEN_DRAWER_OPEN) {
    if (Object.keys(args).sort().join() !== "surface" || typeof args.surface !== "string" || !DRAWER_SURFACE.test(args.surface)) return null;
    return { mode: "drawer", surface: args.surface };
  }
  return Object.keys(args).length === 0 ? { mode: "outlet" } : null;
}

/** CAD-1123 HP3 — the closed verb sets. A verb outside its set closes the
 *  port. `call` verbs never spend or publish: the install's own config and
 *  drafts, and the user's own follow-ups to a confirmed draft (schedule,
 *  reschedule, cancel), run directly; the confirm card is the one approval. Every spend, publish or custody verb is a `slot` verb
 *  and waits for a tap on a host-drawn button. */
export const CALL_VERBS = ["read.run", "context.defaults.save", "open-link", "screen.drawer.open", "screen.drawer.close", "social.drafts.list", "social.drafts.show", "social.drafts.create", "social.drafts.update", "social.drafts.discard", "social.drafts.publish.stage", "social.sources.show", "social.sources.save", "social.destinations.list", "publish.destination.use", "publish.settings.save", "publish.draft.schedule", "publish.draft.reschedule", "publish.draft.cancel", "media.read", "media.publish", "media.chat.attach"] as const;
export const SLOT_VERBS = ["run.start", "publish.draft.confirm", "media.original.download", "social.image.pick"] as const;
/** CAD-1365 — the protected original download slot. Qualified per mount
 *  (screen-media.v1 + media.read + live token); never a generic action verb. */
export const ORIGINAL_DOWNLOAD_VERB = "media.original.download" as const;
/** CAD-1365 — the media contract's verbs parse on the closed call
 *  envelope but are NEVER in the generic advertised set: they are
 *  appended per-mount, only to a media-opted-in child whose minted
 *  receipt declares them. */
export const MEDIA_VERBS = ["media.read", "media.publish", "media.chat.attach"] as const;
export type MediaVerb = (typeof MEDIA_VERBS)[number];
export const isMediaVerb = (verb: string): verb is MediaVerb =>
  (MEDIA_VERBS as readonly string[]).includes(verb);
/** The verbs a screen.v2 PUSH advertises in `actions` — media verbs are
 *  deliberately absent; the channel appends a mount's declared media
 *  rights itself. */
export const ACTION_VERBS: string[] = [...CALL_VERBS.filter(v => !(MEDIA_VERBS as readonly string[]).includes(v)), ...SLOT_VERBS.filter(v => v !== ORIGINAL_DOWNLOAD_VERB)];
export const RECORD_CALL_VERBS = ["records.list", "records.show", "records.create", "records.update", "records.csv.preview", "segments.list", "segments.show", "segments.save", "exclusions.list", "exclusions.show", "exclusions.save", "suppressions.list", "suppressions.add", "audience.preview", "audience.prepare", "audience.show", "content.list", "content.show", "content.save", "content.clone", "content.render", "proposals.list", "proposals.show", "proposals.render", "proposals.apply", "proposals.discard", "sender-bindings.list", "sender-bindings.show", "sender-bindings.save", "content.test-prepare", "content.send-prepare", "sends.prepare", "sends.show", "sends.list", "smtp.show", "origin.show", "campaign-work.show", "campaign-work.save", "screen.preview.show", "screen.preview.clear", "screen.route.patch", "whatsapp.status", "whatsapp.connect_session", "whatsapp.conversations.list", "whatsapp.conversations.show", "whatsapp.messages", "whatsapp.reply.prepare", "whatsapp.reply.status", "whatsapp.templates.list", "whatsapp.template.show", "whatsapp.template.prepare", "whatsapp.template.status", "whatsapp.broadcasts.list", "whatsapp.broadcast.show", "whatsapp.broadcast.estimate", "whatsapp.broadcast.results", "whatsapp.broadcast.draft", "whatsapp.broadcast.test.prepare", "whatsapp.broadcast.prepare", "whatsapp.handoff.set", "chat.whatsapp.open", "whatsapp.draft.show", "whatsapp.draft.discard", "works.query", "works.get", "works.create", "works.patch", "works.trash", "works.restore", "works.request_review"] as const;
export const RECORD_SLOT_VERBS = ["records.csv.import", "records.csv.confirm", "content.approve", "smtp.manage", "smtp.bind", "smtp.rebind", "smtp.revoke", "smtp.test-send", "sends.approve", "sends.resolve", "origin.manage", "chat.campaign.open", "chat.campaign.brief", "whatsapp.reply.send", "whatsapp.template.send", "whatsapp.broadcast.test.send", "whatsapp.broadcast.send", "whatsapp.broadcast.stop", "whatsapp.handoff.release", "chat.whatsapp.brief", "works.review", "works.export"] as const;
export const RECORD_OPERATIONS: readonly string[] = [...RECORD_CALL_VERBS, ...RECORD_SLOT_VERBS];
export type RecordCallVerb = (typeof RECORD_CALL_VERBS)[number];
export type RecordSlotVerb = (typeof RECORD_SLOT_VERBS)[number];
export type CallVerb = (typeof CALL_VERBS)[number] | RecordCallVerb;
export type SlotVerb = (typeof SLOT_VERBS)[number] | RecordSlotVerb;
export interface ScreenNavigation { section_query: string; default_section: string; sections: { id: string; label: string }[] }
export interface ScreenRoute { section: string; view: "list" | "new"; record_id: string | null; segment_id: string | null }
export interface ReadObservation { operation: string; args: ActionArgs; data: unknown }
export const SCREEN_RECORDS_V1 = "screen-records.v1";
/** CRM presentation data only; never an operation or permission. */
export const CRM_LOCALE_V1 = "crm-locale.v1";
/** Larger envelopes are admitted only after verified records negotiation and rights. */
export function recordArgsLimit(verb: string): number {
  return ["records.csv.preview", "records.csv.import", "records.csv.confirm"].includes(verb) ? 327680
    : ["records.create", "records.update", "content.save", "screen.preview.show", "whatsapp.reply.prepare", "whatsapp.reply.send", "whatsapp.template.prepare", "whatsapp.template.send"].includes(verb) ? 49152
    : ["works.create", "works.patch"].includes(verb) ? 81920
    : verb === "whatsapp.broadcast.draft" ? 196608 : ACTION_ARGS_BYTES;
}
export const ACTION_ID = /^[A-Za-z0-9_-]{1,64}$/;
/** A v2 tool alias grammar: lowercase words joined by dots (matches the
 *  declaration). The alias is app vocabulary, never a route/provider name. */
export const TOOL_ALIAS = /^[a-z][a-z0-9]{0,31}(\.[a-z][a-z0-9]{0,31}){0,3}$/;
/** A tool request id — one stable idempotency id per intent. */
export const TOOL_REQUEST_ID = /^[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}$/;
export const ACTION_ARGS_BYTES = 16 * 1024;
/** Where the frame asks the host to draw its button: a rectangle in the
 *  frame's own viewport (host clamps it to the frame), or the host footer bar. */
export type SlotAnchor = { x: number; y: number; w: number; h: number } | "footer";
export type ActionArgs = Record<string, unknown>;
export interface CallRequest { v: 2; op: "call"; id: string; verb: CallVerb; args: ActionArgs }
export interface SlotRequest { v: 2; op: "slot"; id: string; verb: SlotVerb; args: ActionArgs; anchor: SlotAnchor }
/** CAD-1177 — a standalone tool call. `alias` is the app-declared logical
 *  name its `app-screens/v2` `tools` map binds (never a provider/tool/
 *  account/route); `input` is its bounded argument bag; `request_id` is
 *  the stable idempotency id for one intent. The host holds the mount's
 *  action context — the frame never sees it. */
export interface GenerationScope { operation:"caption"|"image"; draft_id:string; revision:number }
export interface ToolRequest { v: 2; op: "tool"; id: string; alias: string; input: ActionArgs; request_id: string; generation_scope?:GenerationScope }
/** A refusal a person can read: a short code and one plain line (no price,
 *  no daemon detail). */
export interface ActionRefusal { code: string; text: string }
export type ActionResult = { ok: true; data: unknown } | { ok: false; refusal: ActionRefusal };
export type SlotStateName = "pending" | "done" | "refused";
/** CAD-1365 — the bounded run-start receipt a `run.start` slot may forward
 *  on `slot-state` done: exactly the native run id, the request id the tap
 *  was planned with, and the native run state (failed/cancelled stay
 *  truthful — done is transport completion, never generation success).
 *  No snapshot, quote, prompt, ref or bytes ever cross this seam. */
export interface RunSlotReceipt {
  run_id: string;
  request_id: string;
  state: "awaiting_approval" | "approved" | "running" | "succeeded" | "failed" | "cancelled";
}
const RUN_SLOT_STATES = ["awaiting_approval", "approved", "running", "succeeded", "failed", "cancelled"] as const;
const RUN_ID = /^run-[0-9a-f]{32}$/;
/** The native request-id grammar: lowercase ASCII alnum/hyphen, non-leading
 *  dash, at most 64 chars — the same bound the tool request id enforces. */
export const RUN_REQUEST_ID = /^[a-z0-9][a-z0-9-]{0,63}$/;
/** Runtime check for a closed receipt: exactly the three fields under the
 *  native grammars. A malformed or extra field is `null` — never silently
 *  stripped into a valid-looking receipt. */
export function parseRunSlotReceipt(value: unknown): RunSlotReceipt | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const r = value as Record<string, unknown>;
  if (Object.keys(r).sort().join() !== "request_id,run_id,state") return null;
  if (typeof r.run_id !== "string" || !RUN_ID.test(r.run_id)) return null;
  if (typeof r.request_id !== "string" || !RUN_REQUEST_ID.test(r.request_id)) return null;
  if (typeof r.state !== "string" || !(RUN_SLOT_STATES as readonly string[]).includes(r.state)) return null;
  return { run_id: r.run_id, request_id: r.request_id, state: r.state as RunSlotReceipt["state"] };
}
export type ReplyMessage =
  | { v: 2; op: "reply"; id: string; ok: true; data: unknown }
  | { v: 2; op: "reply"; id: string; ok: false; refusal: ActionRefusal }
  | { v: 2; op: "tool-reply"; id: string; ok: true; data: unknown }
  | { v: 2; op: "tool-reply"; id: string; ok: false; refusal: ActionRefusal }
  | { v: 2; op: "slot-state"; id: string; state: "done"; receipt?: RunSlotReceipt; data?: never }
  | { v: 2; op: "slot-state"; id: string; state: "done"; data: unknown; receipt?: never }
  | { v: 2; op: "slot-state"; id: string; state: "pending" | "refused"; refusal?: ActionRefusal };

/** CAD-1123 HP1 — the generic read projection v2. A child opts in with
 *  `{v:1, op:"ready", accepts:["screen.v2"]}` (it may also list
 *  `publish-intents.v1`, which v2 includes). Its PUSH is the v1 envelope
 *  with `v: 2`. Only a v2 child may send `{v:2, op:"asset", ref}`, and only
 *  for an `image_ref` the last PUSH carried. */
export const SCREEN_V2 = "screen.v2";
/** CAD-1177 — the tool/action channel a v2 screen declares with
 *  `host_contract: "screen-actions.v1"`. A child opts in by listing it in
 *  `accepts` alongside `screen.v2`; only then may it send `{v:2,op:"tool"}`. */
export const SCREEN_ACTIONS_V1 = "screen-actions.v1";
/** CAD-1615 — the child opts in to being told which surface its frame is
 *  in. A frame that does not advertise it never receives a presentation
 *  message, so an older package is never shown an op it cannot parse. */
export const SCREEN_PRESENTATION_V1 = "screen-presentation.v1";
/** CAD-1777 — the child opts in to the host-owned surface push (role and
 *  modal state), replacing `screen-presentation.v1` for a package that can
 *  draw as a primary or a drawer. A frame that does not advertise it never
 *  receives a surface message. */
export const SCREEN_SURFACES_V1 = "screen-surfaces.v1";
/** CAD-1365 — the media catalog/curation channel a media-contract
 *  screen's child opts into alongside `screen.v2`. Opting in grants
 *  nothing: dispatch still needs the minted receipt's declared
 *  `media.may` permission and its action token. */
export const SCREEN_MEDIA_V1 = "screen-media.v1";
export type Accept = typeof PUBLISH_INTENTS_V1 | typeof SCREEN_V2 | typeof SCREEN_ACTIONS_V1 | typeof SCREEN_MEDIA_V1 | typeof SCREEN_RECORDS_V1 | typeof CRM_LOCALE_V1 | typeof SCREEN_PRESENTATION_V1 | typeof SCREEN_SURFACES_V1;
/** The negotiated PUSH shape: the exact CAD-1006 v1, v1 plus
 *  publish-intents.v1, or the v2 projection. */
export type Shape = "v1" | "intents" | "v2";
/** A workspace screen's shape for its `ready` opt-in, or `null` for an
 *  opt-in it cannot honour (the chat-only `chat-directive.v1`). */
export function shapeOf(accepts: Accept[] | [typeof CHAT_DIRECTIVE_V1] | undefined): Shape | null {
  if (!accepts) return "v1";
  if (accepts.some(a => a === CHAT_DIRECTIVE_V1)) return null;
  return accepts.some(a => a === SCREEN_V2) ? "v2" : "intents";
}
/** Whether a ready opt-in includes the tool/action channel. The media
 *  contract keeps the declared tool/action handshake, so a media opt-in
 *  counts as the action channel too. */
export function wantsActions(accepts: readonly string[] | undefined): boolean {
  return !!accepts && (accepts.includes(SCREEN_ACTIONS_V1) || accepts.includes(SCREEN_MEDIA_V1));
}
/** Whether a ready opt-in includes the media channel (screen.v2 is
 *  required anyway — media never negotiates under an older shape). */
export function wantsMedia(accepts: readonly string[] | undefined): boolean {
  return !!accepts && accepts.includes(SCREEN_MEDIA_V1);
}

/** CAD-1365 — closed media.call arg parsers. `kind:"asset"` detail and
 *  `media.generate` are recognized-but-unadmitted: they refuse here, so
 *  nothing strips or forwards them. Any extra field — including a
 *  forged authority/scope selector — refuses outright, never silently
 *  strips. Exported so the dispatcher re-validates a direct call.
 *  `kind:"catalog"` only: `{kind}` or `{kind,limit,cursor}` with
 *  `limit` a safe integer 1..20 and `cursor` a string <= 160 chars. */
export function mediaReadArgs(args: ActionArgs): ActionArgs | null {
  const keys = Object.keys(args).sort().join();
  if (args.kind === "preview") {
    if (Object.keys(args).sort().join() !== "asset_id,expected_revision,kind") return null;
    if (typeof args.asset_id !== "string" || !MEDIA_ASSET_ID.test(args.asset_id)) return null;
    if (typeof args.expected_revision !== "number" || !Number.isSafeInteger(args.expected_revision) || args.expected_revision < 1) return null;
    return args;
  }
  if (keys !== "kind" && keys !== "kind,limit" && keys !== "cursor,kind" && keys !== "cursor,kind,limit") return null;
  if (args.kind !== "catalog") return null;
  if ("limit" in args && (typeof args.limit !== "number" || !Number.isSafeInteger(args.limit) || args.limit < 1 || args.limit > 20)) return null;
  if ("cursor" in args && (typeof args.cursor !== "string" || args.cursor.length > 160)) return null;
  return args;
}
const MEDIA_ASSET_ID = /^med-[0-9a-f]{32}$/;
const MEDIA_REQUEST_ID = /^[A-Za-z0-9_-]{1,128}$/;
/** `media.original.download` args are EXACTLY `{asset_id}`, a canonical id. */
export function mediaChatAttachArgs(args: ActionArgs): ActionArgs | null {
  if (Object.keys(args).sort().join() !== "asset_id,expected_revision") return null;
  if (typeof args.asset_id !== "string" || !MEDIA_ASSET_ID.test(args.asset_id)) return null;
  if (typeof args.expected_revision !== "number" || !Number.isSafeInteger(args.expected_revision) || args.expected_revision < 1) return null;
  return args;
}
export function originalDownloadArgs(args: ActionArgs): ActionArgs | null {
  if (Object.keys(args).join() !== "asset_id") return null;
  if (typeof args.asset_id !== "string" || !MEDIA_ASSET_ID.test(args.asset_id)) return null;
  return args;
}
/** `media.publish` args are EXACTLY `{asset_id,expected_revision,
 *  request_id}` under the frozen publication contract — a canonical
 *  `med-` + 32-hex asset id, a positive safe-integer revision, an ASCII
 *  alnum/-/_ request id 1..128. No frame-supplied token, scope, creator,
 *  session or URL field ever parses. */
export function mediaPublishArgs(args: ActionArgs): ActionArgs | null {
  if (Object.keys(args).sort().join() !== "asset_id,expected_revision,request_id") return null;
  if (typeof args.asset_id !== "string" || !MEDIA_ASSET_ID.test(args.asset_id)) return null;
  if (typeof args.expected_revision !== "number" || !Number.isSafeInteger(args.expected_revision) || args.expected_revision < 1) return null;
  if (typeof args.request_id !== "string" || !MEDIA_REQUEST_ID.test(args.request_id)) return null;
  return args;
}
/** An asset ref is host-minted (`image:<run id>`), never a receipt id. */
export const ASSET_REF = /^(?:image:[A-Za-z0-9_-]{1,128}|draft-image:[A-Za-z0-9_-]{1,128}:[a-z][a-z0-9]{0,31}(?:\.[a-z][a-z0-9]{0,31}){0,3}|upload-image:[A-Za-z0-9_-]{1,128}:[a-z][a-z0-9]{0,31}(?:\.[a-z][a-z0-9]{0,31}){0,3})$/;
/** The host-held draft image ref: draft id and the tool alias that reads it. */
const DRAFT_IMAGE_REF = /^draft-image:([A-Za-z0-9_-]{1,128}):([a-z][a-z0-9]{0,31}(?:\.[a-z][a-z0-9]{0,31}){0,3})$/;
export function parseDraftImageRef(ref: string): { draftId: string; alias: string } | null {
  const match = DRAFT_IMAGE_REF.exec(ref);
  return match ? { draftId: match[1], alias: match[2] } : null;
}
/** SOC-32: the retained upload preview ref. The id is the host-minted asset
 *  receipt, never a path; the alias names the live screen's image slot. */
const UPLOAD_IMAGE_REF = /^upload-image:([A-Za-z0-9_-]{1,128}):([a-z][a-z0-9]{0,31}(?:\.[a-z][a-z0-9]{0,31}){0,3})$/;
export function parseUploadImageRef(ref: string): { assetId: string; alias: string } | null {
  const match = UPLOAD_IMAGE_REF.exec(ref);
  return match ? { assetId: match[1], alias: match[2] } : null;
}
/** The host's one reply to an asset request: a downscaled data URL. A ref
 *  the host cannot load now gets no reply (the frame keeps its placeholder). */
export type AssetReply = { v: 2; op: "asset"; ref: string; data_url: string };
/** Each asset reply's data URL is at most this many bytes (96 KiB), of an
 *  image no larger than 512 px on its longer side. */
export const ASSET_BYTES_MAX = 96 * 1024;
export const ASSET_PX_MAX = 512;
export const ASSET_DATA_URL = /^data:image\/(jpeg|png|webp);base64,[A-Za-z0-9+/]+=*$/;

/** CAD-1025 — the one optional PUSH extension. A child opts in by sending
 *  `{v:1, op:"ready", accepts:["publish-intents.v1"]}`; a child that sends
 *  the bare `ready` keeps receiving the exact CAD-1006 v1 shape. */
export const PUBLISH_INTENTS_V1 = "publish-intents.v1";
/** CAD-1110 — the chat-only opt-in: a screen mounted inline in chat sends
 *  `{v:1, op:"ready", accepts:["chat-directive.v1"]}` and is pushed ONLY the
 *  directive (see `ChatDirectivePush`), never the scope projection above. */
export const CHAT_DIRECTIVE_V1 = "chat-directive.v1";

/** The one push a chat frame receives: the matched kind plus the flat,
 *  bounded, identity-free payload the host's matcher validated. */
export interface ChatDirectivePush {
  v: 1;
  op: "directive";
  tag: string;
  kind: string;
  data: Record<string, string | number | boolean>;
}
/** The whole directive push is at most this many UTF-8 bytes. */
export const CHAT_PUSH_BYTES_MAX = 4 * 1024;
/** Every PUSH a child receives is at most this many UTF-8 bytes. */
export const PUSH_BYTES_MAX = 128 * 1024;
const LINK_ID_MAX = 128;

/** One verified publish intent, read-only. Every field is required; there is
 *  no grant, approval, idempotency key, receipt, upstream evidence, caption or
 *  image digest. `context_id` `""` means no context. */
export interface ScreenIntent {
  intent_id: string;
  install_id: string;
  context_id: string;
  run_id: string;
  effect_id: string;
  state: "queued" | "processing" | "posted" | "refused" | "cancelled" | "held";
  channel: "instagram" | "facebook";
  destination_id: string;
  due_epoch: number;
  timezone: string;
  /** screen.v2 only, omitted when none: why the send was refused or held. */
  refusal?: ScreenRefusal;
  /** screen.v2 only, omitted when none: the posted https Instagram permalink. */
  permalink?: string;
}

/** A refusal reason a person can read: a short code and one plain line. */
export interface ScreenRefusal { code: string; text: string }

/** screen.v2 — one run's host-derived progress and material, read-only.
 *  A field the host cannot state is omitted, never invented. */
export interface ScreenRunV2 {
  /** Run epochs in seconds; `closed` only once the run is terminal. */
  created?: number;
  closed?: number;
  /** awaiting | working:<step kind> | checking | ready | failed | cancelled */
  phase: string;
  steps?: { id: string; kind: string; state: string }[];
  /** Effective values of the workflow's `context_default` inputs, with origin. */
  inputs_used?: Record<string, { value: string; origin: string }>;
  /** First ≤ 600 chars of the reviewer-approved text artifact. */
  caption_excerpt?: string;
  artifact_id?: string;
  review?: { decision: string; rationale: string };
  approved?: { by_display: string; at: number };
  source_post_id?: string;
  refusal?: ScreenRefusal;
  /** Host-minted ref (`image:<run id>`) for the reviewed generated image;
   *  the only thing a child may request over the asset channel. */
  image_ref?: string;
}
export interface ScreenSourcePost {
  id: string; caption: string; published_at: number; permalink: string;
  media_kind: string;
  /** https URL on the Instagram CDN. Only a screen whose declaration opts
   *  into `remote_images: ["instagram-cdn"]` may load it (frame CSP). */
  thumb_url?: string;
}
export interface ScreenSources {
  handle: string; fetched_at?: number;
  posts: ScreenSourcePost[];
  /** Set when the library came from a run-bound source read. */
  run_id?: string;
  /** Set instead of run_id when the library came from a standalone tool
   *  invocation (CAD-1177) — the retained app_tool receipt id, distinct
   *  from any run id. A screen keys refresh/edit to this, never a run. */
  tool_receipt_id?: string;
}
export interface ScreenPublishSettings {
  revision: number;
  destination_id?: string;
  destination_label?: string;
  toolkit?: "instagram" | "facebook";
  timezone?: string;
}
export interface ScreenDefaults {
  context_id: string; revision: number;
  values: Record<string, string>;
}

/** `ok`: the complete scoped list (empty rows = honestly nothing planned).
 *  `truncated`: the list hit its cap, so absence proves nothing.
 *  `loading`/`unavailable`: no verified read for this scope; rows are empty.
 *  `withheld` counts host-dropped rows (foreign, dangling, malformed, or a
 *  duplicated id), never their content. */
export interface ScreenIntents {
  status: "loading" | "ok" | "truncated" | "unavailable";
  withheld: number;
  rows: ScreenIntent[];
}

/** The host's only PUSH: the closed read-only projection of the verified
 *  scope. Every identity field is host-stamped — the child can never
 *  choose or widen scope. Fields that are unavailable are omitted, never
 *  fabricated: a run with no real timestamps omits them, and artifact
 *  bytes are absent in MVP rather than filled with placeholders. */
export interface ScreenPush {
  /** 2 for a screen.v2 child, 1 otherwise. */
  v: 1 | 2;
  op: "screen";
  /** Host-stamped install id — the verified route's install. */
  install_id: string;
  /** Live bundle digest the mount was proved against. */
  digest: string;
  /** The mounted screen tag. */
  tag: string;
  /** Host-stamped context id; `""` = no context scope. */
  context_id: string;
  /** Verified installation receipt fields, minimally. */
  installation: {
    install_id: string;
    name: string;
    title: string;
    version: string;
    digest: string;
    summary: string;
  };
  /** Active contexts as `{id,label}` only. */
  contexts: { id: string; label: string }[];
  /** Runs inside the pushed scope — real ids/states/titles only. A run's
   *  `created`/`closed` are real epoch seconds or omitted when unknown. */
  runs: ({
    id: string;
    state: string;
    title: string;
    snapshot_digest: string;
    /** v2 adds `name` (the installed workflow file, `null` when the
     *  workflow changed since) and `kind` (`read` | `draft`). */
    workflow: { title: string; name?: string | null; kind?: string };
    created?: number;
    closed?: number;
    /** publish-intents.v1 only: the run's context, `""` = none. */
    context_id?: string;
  } & Partial<ScreenRunV2>)[];
  /** Scoped effects/outbox/calendar-intent receipts — real rows only. */
  outbox: {
    effect_id: string;
    state: string;
    title: string;
    scheduled_at?: number;
    /** publish-intents.v1 only: the effect's full linkage identity. */
    run_id?: string;
    context_id?: string;
  }[];
  /** publish-intents.v1 only. */
  publish_intents?: ScreenIntents;
  /** screen.v2 only: the latest finished read of a source slot. */
  sources?: ScreenSources;
  /** screen.v2 only: effective `context_default` values for this scope. */
  defaults?: ScreenDefaults;
  /** screen.v2 only (CAD-1902): the bound publication target, read-only.
   *  `revision` is the binding revision; every other field is omitted when
   *  it does not pass the app's grammar. A grant is never pushed. */
  publish_settings?: ScreenPublishSettings;
  /** screen.v2 only: plain blocker codes; `ok` when there are none. */
  readiness?: { ok: boolean; blockers: string[] };
  /** screen.v2 only: verbs this host lets this viewer's frame use (none for a non-operator). */
  actions?: string[];
  /** screen.v2 only, host-derived: `1` tells the child this mount's slot
   *  replies may carry a `RunSlotReceipt` for a `run.start` it sends. Set
   *  only by the sender when a real planner and a projected `run.start`
   *  action exist — never forwarded from input, and stripped for a
   *  legacy/intents child. */
  run_receipts?: 1;
  /** screen.v2 only, host-derived like `run_receipts`: `1` when this mount may
   *  use the original download slot. Support only, never permission. */
  original_downloads?: 1;
  /** Host marker. 1 only when this mount's media.read preview is admitted. */
  media_previews?: 1;
  /** Host marker. 1 only when this mount may media.chat.attach. */
  media_chat_references?: 1;
  /** Server epoch seconds for day-row alignment. */
  now: number;
  /** The child's own last `state` snapshot, replayed on remount. */
  resume?: string;
  /** Records-negotiated display only, never authority. */
  route?: ScreenRoute;
  read_observations?: ReadObservation[];
  /** Host-selected presentation only, for CRM records peers opting into crm-locale.v1. */
  presentation_locale?: "en" | "zh-TW";
}

export type HostToChild = ScreenPush | AssetReply | ReplyMessage | ScreenSurface;

/** Parse a window message into a `ScreenInit`, or `null`. The port is a
 *  `MessagePort` in `event.ports[0]` — the caller checks `event.origin`
 *  and `event.source` first; this only checks the envelope shape. */
export function parseInit(data: unknown): ScreenInit | null {
  if (typeof data !== "object" || data === null || Array.isArray(data)) return null;
  const d = data as Record<string, unknown>;
  if (
    Object.keys(d).sort().join() === "bridge_nonce,generation,op,tag,v" &&
    d.v === 1 &&
    d.op === "init" &&
    typeof d.tag === "string" && /^[a-z0-9][a-z0-9-]{0,31}$/.test(d.tag) &&
    typeof d.bridge_nonce === "string" && /^[a-f0-9]{64}$/.test(d.bridge_nonce) &&
    typeof d.generation === "number" &&
    Number.isSafeInteger(d.generation) &&
    d.generation >= 0
  ) {
    return {
      v: 1,
      op: "init",
      tag: d.tag,
      bridge_nonce: d.bridge_nonce,
      generation: d.generation,
    };
  }
  return null;
}

/** A plain-object argument bag within its byte bound (JSON-only: no function, undefined or cycle). */
function generationScope(value:unknown):GenerationScope|null {
  if(!value||typeof value!=="object"||Array.isArray(value))return null;
  const s=value as Record<string,unknown>, keys=Object.keys(s).sort().join();
  if(keys!=="draft_id,operation,revision")return null;
  if(s.operation!=="caption"&&s.operation!=="image")return null;
  if(typeof s.draft_id!=="string"||!ACTION_ID.test(s.draft_id)||typeof s.revision!=="number"||!Number.isSafeInteger(s.revision)||s.revision<1)return null;
  return s as unknown as GenerationScope;
}
function actionArgs(value: unknown, limit = ACTION_ARGS_BYTES): ActionArgs | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  try {
    const text = JSON.stringify(value);
    return text !== undefined && new TextEncoder().encode(text).byteLength <= limit ? value as ActionArgs : null;
  } catch { return null; }
}
const COORD_MAX = 16384;
function slotAnchor(value: unknown): SlotAnchor | null {
  if (value === "footer") return "footer";
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const a = value as Record<string, unknown>;
  if (Object.keys(a).sort().join() !== "h,w,x,y") return null;
  const [x, y, w, h] = [a.x, a.y, a.w, a.h];
  if (![x, y, w, h].every(n => typeof n === "number" && Number.isFinite(n) && n >= 0 && n <= COORD_MAX) ||
      (w as number) <= 0 || (h as number) <= 0) return null;
  return { x: x as number, y: y as number, w: w as number, h: h as number };
}

/** Parse a port message into a `ChildToHost`, or `null` (caller closes
 *  the port). `state.data` is bounded at ≤ 32 KiB here so a hostile or
 *  buggy child cannot push an unbounded snapshot. `drawer` is the live
 *  host presentation capability; it is only ever true (or a verb subset)
 *  for a records frame that negotiated `screen-presentation.v1` or
 *  `screen-surfaces.v1` behind a shell drawer. A primary admits both
 *  closed presentation verbs; a drawer frame admits only
 *  `screen.drawer.close`, so it can never open another drawer. The verbs
 *  are never the generic call set and never a records operation. */
export function parseChild(data: unknown, records?: readonly string[], drawer: boolean | readonly ScreenDrawerVerb[] = false): ChildToHost | null {
  if (typeof data !== "object" || data === null || Array.isArray(data)) return null;
  const d = data as Record<string, unknown>;
  if (Object.keys(d).sort().join() === "op,v" && d.v === 1 && d.op === "ready") {
    return { v: 1, op: "ready" };
  }
  if (
    Object.keys(d).sort().join() === "accepts,op,v" && d.v === 1 && d.op === "ready" &&
    Array.isArray(d.accepts) && d.accepts.length >= 1 && d.accepts.length <= 4 &&
    new Set(d.accepts).size === d.accepts.length &&
    d.accepts.every(a => a === PUBLISH_INTENTS_V1 || a === SCREEN_V2 || a === SCREEN_ACTIONS_V1 || a === SCREEN_MEDIA_V1 || a === SCREEN_RECORDS_V1 || a === CRM_LOCALE_V1 || a === SCREEN_PRESENTATION_V1 || a === SCREEN_SURFACES_V1)
  ) {
    return { v: 1, op: "ready", accepts: [...d.accepts] as Accept[] };
  }
  if (Object.keys(d).sort().join() === "op,ref,v" && d.v === 2 && d.op === "asset" &&
      typeof d.ref === "string" && ASSET_REF.test(d.ref)) {
    return { v: 2, op: "asset", ref: d.ref };
  }
  if (d.v === 2 && d.op === "call" && Object.keys(d).sort().join() === "args,id,op,v,verb") {
    // CAD-1777: the two host-local presentation verbs parse for a records
    // frame only when the host presentation capability is live. The minted
    // receipt's records operations never include them, so they are admitted
    // beside (never from) the receipt's rights — and the generic call set
    // stays closed to a records frame either way.
    const drawerVerbs: readonly ScreenDrawerVerb[] = drawer === true ? SCREEN_DRAWER_VERBS : drawer === false ? [] : drawer;
    // A drawer frame's admitted subset is exactly `[close]`, so
    // `screen.drawer.open` from one never parses and cannot recurse.
    const hostVerb = drawerVerbs.length > 0 && records !== undefined && typeof d.verb === "string" &&
      isScreenDrawerVerb(d.verb) && drawerVerbs.includes(d.verb);
    const allowed = records ? RECORD_CALL_VERBS.filter(v => records.includes(v)) : CALL_VERBS;
    const admitted = hostVerb || (allowed as readonly unknown[]).includes(d.verb);
    const args = actionArgs(d.args, records && admitted && !hostVerb ? recordArgsLimit(d.verb as string) : ACTION_ARGS_BYTES);
    if (typeof d.id !== "string" || !ACTION_ID.test(d.id) || !admitted || !args) return null;
    // A host-local presentation verb's exact argument shape is enforced by
    // the dispatch guard (`parseScreenDrawerCall`), so a malformed request is
    // refused with `bad_args` and the live frame survives — it never changes
    // geometry and never retires the records bridge.
    if (hostVerb) return { v: 2, op: "call", id: d.id, verb: d.verb as ScreenDrawerVerb, args };
    // Media verbs carry their own CLOSED arg shapes — parsed exactly,
    // refused outright otherwise (unadmitted kinds, unbounded values and
    // forged authority fields never reach the channel's call path).
    if (d.verb === "media.read") {
      const checked = mediaReadArgs(args);
      return checked ? { v: 2, op: "call", id: d.id, verb: "media.read", args: checked } : null;
    }
    if (d.verb === "media.chat.attach") {
      const checked = mediaChatAttachArgs(args);
      return checked ? { v: 2, op: "call", id: d.id, verb: "media.chat.attach", args: checked } : null;
    }
    if (d.verb === "media.publish") {
      const checked = mediaPublishArgs(args);
      return checked ? { v: 2, op: "call", id: d.id, verb: "media.publish", args: checked } : null;
    }
    return { v: 2, op: "call", id: d.id, verb: d.verb as CallVerb, args };
  }
  if (d.v === 2 && d.op === "slot" && Object.keys(d).sort().join() === "anchor,args,id,op,v,verb") {
    const allowed = records ? RECORD_SLOT_VERBS.filter(v => records.includes(v)) : SLOT_VERBS;
    const admitted = (allowed as readonly unknown[]).includes(d.verb);
    const args = actionArgs(d.args, records && admitted ? recordArgsLimit(d.verb as string) : ACTION_ARGS_BYTES);
    const anchor = slotAnchor(d.anchor);
    if (typeof d.id === "string" && ACTION_ID.test(d.id) && admitted && args && anchor &&
        (d.verb !== ORIGINAL_DOWNLOAD_VERB || originalDownloadArgs(args)))
      return { v: 2, op: "slot", id: d.id, verb: d.verb as SlotVerb, args, anchor };
    return null;
  }
  if (d.v === 2 && d.op === "tool" && ["alias,id,input,op,request_id,v","alias,generation_scope,id,input,op,request_id,v"].includes(Object.keys(d).sort().join())) {
    const input = actionArgs(d.input);
    const scoped=d.generation_scope===undefined?undefined:generationScope(d.generation_scope);
    if (typeof d.id === "string" && ACTION_ID.test(d.id) &&
        typeof d.alias === "string" && TOOL_ALIAS.test(d.alias) &&
        typeof d.request_id === "string" && TOOL_REQUEST_ID.test(d.request_id) && input && (d.generation_scope===undefined||scoped))
      return { v: 2, op: "tool", id: d.id, alias: d.alias, input, request_id: d.request_id,...(scoped?{generation_scope:scoped}:{}) };
    return null;
  }
  if (
    Object.keys(d).sort().join() === "accepts,op,v" && d.v === 1 && d.op === "ready" &&
    Array.isArray(d.accepts) && d.accepts.length === 1 && d.accepts[0] === CHAT_DIRECTIVE_V1
  ) {
    return { v: 1, op: "ready", accepts: [CHAT_DIRECTIVE_V1] };
  }
  if (
    Object.keys(d).sort().join() === "data,op,v" &&
    d.v === 1 &&
    d.op === "state" &&
    typeof d.data === "string" &&
    d.data.length <= 32 * 1024 &&
    new TextEncoder().encode(d.data).byteLength <= 32 * 1024
  ) {
    return { v: 1, op: "state", data: d.data };
  }
  return null;
}

const V2_TOP = ["sources", "defaults", "publish_settings", "readiness", "actions", "run_receipts", "original_downloads", "media_previews", "media_chat_references", "route", "read_observations", "presentation_locale"] as const;
const V2_RUN = ["created", "closed", "phase", "steps", "inputs_used", "caption_excerpt", "artifact_id",
  "review", "approved", "source_post_id", "refusal", "image_ref"] as const;
/** The publish-intents.v1 shape: every screen.v2 field is removed. */
export function intentsPush(push: ScreenPush): ScreenPush {
  const top: Record<string, unknown> = { ...push, v: 1 };
  for (const key of V2_TOP) delete top[key];
  const out = top as unknown as ScreenPush;
  out.runs = push.runs.map(run => {
    const row: Record<string, unknown> = { ...run, workflow: { title: run.workflow.title } };
    for (const key of V2_RUN) delete row[key];
    return row as ScreenPush["runs"][number];
  });
  if (push.publish_intents) out.publish_intents = { ...push.publish_intents,
    rows: push.publish_intents.rows.map(({ refusal: _r, permalink: _p, ...row }) => row) };
  return out;
}
/** The exact CAD-1006 v1 shape for a child that did not opt in: every
 *  publish-intents.v1 and screen.v2 field is removed, nothing else changes. */
export function legacyPush(push: ScreenPush): ScreenPush {
  const { publish_intents: _intents, ...rest } = intentsPush(push);
  return {
    ...rest,
    runs: rest.runs.map(({ context_id: _context, ...run }) => run),
    outbox: rest.outbox.map(({ run_id: _run, context_id: _context, ...row }) => row),
  };
}

/** The screen.v2 sections, dropped in this order when the PUSH is over its
 *  byte cap: each step keeps the screen, and an absent section reads as
 *  unavailable to the frame. */
const omit = (key: string) => (p: ScreenPush): ScreenPush => ({ ...p, runs: p.runs.map(r => {
  const row: Record<string, unknown> = { ...r }; delete row[key]; return row as ScreenPush["runs"][number]; }) });
const DEGRADE: ((push: ScreenPush) => ScreenPush)[] = [
  omit("caption_excerpt"),
  ({ sources: _s, ...p }) => p,
  omit("inputs_used"),
  p => ({ ...p, publish_intents: { status: "unavailable", withheld: 0, rows: [] } }),
  omit("steps"),
  omit("review"),
];

export function pushBytes(push: ScreenPush): number {
  return new TextEncoder().encode(JSON.stringify(push)).byteLength;
}
const linkId = (value: string | undefined, required: boolean) =>
  typeof value === "string" && value.length <= LINK_ID_MAX && (!required || value.length > 0);

/** The exact PUSH one child receives, or `null` when it cannot be sent (the
 *  caller then closes the mount and the board shows its native workspace).
 *  The byte cap is checked on THIS shape, so a legacy child is never refused
 *  for bytes only an opted-in child would get. For an opted-in child, the
 *  linkage ids must meet the app's bounds (fail closed otherwise), and when
 *  the intent rows alone push it over the cap they are replaced by an honest
 *  `unavailable` block rather than dropping the screen. */
export function shapeFor(push: ScreenPush, level: boolean | Shape): ScreenPush | null {
  const shape: Shape = level === true ? "intents" : level === false ? "v1" : level;
  if (shape === "v1") {
    const legacy = legacyPush(push);
    return pushBytes(legacy) <= PUSH_BYTES_MAX ? legacy : null;
  }
  if (!push.publish_intents ||
      !push.runs.every(run => linkId(run.context_id, false)) ||
      !push.outbox.every(row => linkId(row.run_id, true) && linkId(row.context_id, false))) return null;
  if (shape === "intents") {
    const intents = intentsPush(push);
    if (pushBytes(intents) <= PUSH_BYTES_MAX) return intents;
    const degraded: ScreenPush = { ...intents, publish_intents: { status: "unavailable", withheld: 0, rows: [] } };
    return pushBytes(degraded) <= PUSH_BYTES_MAX ? degraded : null;
  }
  if (!push.runs.every(run => typeof run.phase === "string")) return null;
  let v2: ScreenPush = { ...push, v: 2 };
  for (const degrade of DEGRADE) {
    if (pushBytes(v2) <= PUSH_BYTES_MAX) return v2;
    v2 = degrade(v2);
  }
  return pushBytes(v2) <= PUSH_BYTES_MAX ? v2 : null;
}

/** The image refs one sent PUSH carries — the only refs its child may ask for. */
export function pushedAssetRefs(push: ScreenPush): Set<string> {
  return new Set(push.runs.map(run => run.image_ref).filter((ref): ref is string => typeof ref === "string"));
}
