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

/** CAD-1123 HP3 — the closed verb sets. A verb outside its set closes the
 *  port. `call` verbs never spend or publish; every spend or publish verb is a
 *  `slot` verb and waits for a tap on a host-drawn button. The `publish.*` slot
 *  verbs arrive with HP4: add them here and in the host's dispatch table. */
export const CALL_VERBS = ["read.run", "context.defaults.save", "open-link", "social.drafts.list", "social.drafts.show", "social.drafts.create", "social.drafts.update", "social.drafts.publish.stage", "social.sources.show", "social.sources.save", "social.destinations.list"] as const;
export const SLOT_VERBS = ["run.start", "publish.destination.use"] as const;
export type CallVerb = (typeof CALL_VERBS)[number];
export type SlotVerb = (typeof SLOT_VERBS)[number];
/** The verbs a screen.v2 PUSH advertises in `actions`. */
export const ACTION_VERBS: string[] = [...CALL_VERBS, ...SLOT_VERBS];
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
export type ReplyMessage =
  | { v: 2; op: "reply"; id: string; ok: true; data: unknown }
  | { v: 2; op: "reply"; id: string; ok: false; refusal: ActionRefusal }
  | { v: 2; op: "tool-reply"; id: string; ok: true; data: unknown }
  | { v: 2; op: "tool-reply"; id: string; ok: false; refusal: ActionRefusal }
  | { v: 2; op: "slot-state"; id: string; state: SlotStateName; refusal?: ActionRefusal };

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
export type Accept = typeof PUBLISH_INTENTS_V1 | typeof SCREEN_V2 | typeof SCREEN_ACTIONS_V1;
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
/** Whether a ready opt-in includes the tool/action channel. */
export function wantsActions(accepts: readonly string[] | undefined): boolean {
  return !!accepts && accepts.includes(SCREEN_ACTIONS_V1);
}
/** An asset ref is host-minted (`image:<run id>`), never a receipt id. */
export const ASSET_REF = /^(?:image:[A-Za-z0-9_-]{1,128}|draft-image:[A-Za-z0-9_-]{1,128}:[a-z][a-z0-9]{0,31}(?:\.[a-z][a-z0-9]{0,31}){0,3})$/;
/** The host-held draft image ref: draft id and the tool alias that reads it. */
const DRAFT_IMAGE_REF = /^draft-image:([A-Za-z0-9_-]{1,128}):([a-z][a-z0-9]{0,31}(?:\.[a-z][a-z0-9]{0,31}){0,3})$/;
export function parseDraftImageRef(ref: string): { draftId: string; alias: string } | null {
  const match = DRAFT_IMAGE_REF.exec(ref);
  return match ? { draftId: match[1], alias: match[2] } : null;
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
  /** screen.v2 only: plain blocker codes; `ok` when there are none. */
  readiness?: { ok: boolean; blockers: string[] };
  /** screen.v2 only: verbs this host lets this viewer's frame use (none for a non-operator). */
  actions?: string[];
  /** Server epoch seconds for day-row alignment. */
  now: number;
  /** The child's own last `state` snapshot, replayed on remount. */
  resume?: string;
}

export type HostToChild = ScreenPush | AssetReply | ReplyMessage;

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
function actionArgs(value: unknown): ActionArgs | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  try {
    const text = JSON.stringify(value);
    return text !== undefined && new TextEncoder().encode(text).byteLength <= ACTION_ARGS_BYTES ? value as ActionArgs : null;
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
 *  buggy child cannot push an unbounded snapshot. */
export function parseChild(data: unknown): ChildToHost | null {
  if (typeof data !== "object" || data === null || Array.isArray(data)) return null;
  const d = data as Record<string, unknown>;
  if (Object.keys(d).sort().join() === "op,v" && d.v === 1 && d.op === "ready") {
    return { v: 1, op: "ready" };
  }
  if (
    Object.keys(d).sort().join() === "accepts,op,v" && d.v === 1 && d.op === "ready" &&
    Array.isArray(d.accepts) && d.accepts.length >= 1 && d.accepts.length <= 3 &&
    new Set(d.accepts).size === d.accepts.length &&
    d.accepts.every(a => a === PUBLISH_INTENTS_V1 || a === SCREEN_V2 || a === SCREEN_ACTIONS_V1)
  ) {
    return { v: 1, op: "ready", accepts: [...d.accepts] as Accept[] };
  }
  if (Object.keys(d).sort().join() === "op,ref,v" && d.v === 2 && d.op === "asset" &&
      typeof d.ref === "string" && ASSET_REF.test(d.ref)) {
    return { v: 2, op: "asset", ref: d.ref };
  }
  if (d.v === 2 && d.op === "call" && Object.keys(d).sort().join() === "args,id,op,v,verb") {
    const args = actionArgs(d.args);
    if (typeof d.id === "string" && ACTION_ID.test(d.id) && (CALL_VERBS as readonly unknown[]).includes(d.verb) && args)
      return { v: 2, op: "call", id: d.id, verb: d.verb as CallVerb, args };
    return null;
  }
  if (d.v === 2 && d.op === "slot" && Object.keys(d).sort().join() === "anchor,args,id,op,v,verb") {
    const args = actionArgs(d.args);
    const anchor = slotAnchor(d.anchor);
    if (typeof d.id === "string" && ACTION_ID.test(d.id) && (SLOT_VERBS as readonly unknown[]).includes(d.verb) && args && anchor)
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

const V2_TOP = ["sources", "defaults", "readiness", "actions"] as const;
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
