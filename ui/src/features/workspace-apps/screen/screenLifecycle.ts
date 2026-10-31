import { ASSET_BYTES_MAX, ASSET_DATA_URL, ORIGINAL_DOWNLOAD_VERB, parseChild, parseInit, pushedAssetRefs, shapeFor, shapeOf,
  wantsActions, wantsMedia, isMediaVerb, isScreenDrawerVerb, parseScreenDrawerCall, SCREEN_DRAWER_CLOSE, SCREEN_DRAWER_VERBS, SCREEN_MEDIA_V1, SCREEN_SURFACES_V1, TOOL_ALIAS, ASSET_REF, SCREEN_RECORDS_V1, CRM_LOCALE_V1, SCREEN_PRESENTATION_V1, SCREEN_V2, RECORD_OPERATIONS, RECORD_CALL_VERBS, RECORD_SLOT_VERBS, CALL_VERBS, SLOT_VERBS,
  type ScreenNavigation,
  type ActionResult, type AssetReply, type CallRequest, type CallVerb, type ChildToHost, type MediaVerb, type ReplyMessage, type ScreenPush,
  type ScreenDrawerVerb, type ScreenDrawerSize, type ScreenPresentation, type ScreenPresentationMode, type ScreenSurface, type ScreenSurfaceRole, type Shape, type SlotRequest, type ToolRequest } from "./screenProtocol";
import { SlotController, type Planner, type SlotView } from "./screenSlot";

/** Resolves one pushed asset ref to a downscaled `data:` URL, or `null`
 *  when it cannot be loaded now. Supplied by the board; never by the frame. */
export type AssetLoader = (ref: string) => Promise<string | null>;

export interface MediaChatAttachArgs {
  asset_id: string;
  expected_revision: number;
}
export interface MediaChatAttached {
  asset_id: string;
  revision: number;
}

/** Private host handoff: the frame cannot choose a conversation or supply a pin. */
export type MediaChatAttach = (
  args: MediaChatAttachArgs,
  actionToken: string,
  isCurrent: () => boolean,
) => Promise<MediaChatAttached>;
/** Most asset requests one frame may have outstanding (the package's
 *  per-mount budget); one more closes it. At most ASSET_PARALLEL load at once. */
export const ASSET_QUEUE_MAX = 256;
const ASSET_PARALLEL = 4;

/** What the board lends one mounted frame for CAD-1123 HP3 actions. Absent
 *  (a non-operator view, a chat frame) the `call` and `slot` ops close the port. */
export interface ScreenActions {
  /** Run a non-spending verb as the board's operator session. */
  call(verb: CallVerb, args: Record<string, unknown>, ui: {
    showLink(url: string): void;
    actionToken?: string;
    signal?: AbortSignal;
    attachMedia?: (args: MediaChatAttachArgs, actionToken: string) => Promise<MediaChatAttached>;
    /** CAD-1615 — the app asks for the board-wide drawer; the shell owns
     *  the geometry. Absent (no shell behind this frame) the verb refuses. */
    openScreenDrawer?(surface: string): boolean;
    closeScreenDrawer?(): boolean;
  }): Promise<ActionResult>;
  /** Validate a spend verb, fetch what the host must check, and name the work. */
  planner: Planner;
  /** The host-drawn slot to show (or hide, `null`). */
  onSlot(view: SlotView | null): void;
  onLink(url: string): void;
  /** CAD-1615 / CAD-1777 — the shell presents this frame in the
   *  board-wide drawer (`"drawer"`) or back in the app outlet (`"outlet"`).
   *  `independent` is stamped by the host from the negotiated
   *  `screen-surfaces.v1` capability: an independent open mounts a separate
   *  drawer frame in the shell's own fixed overlay instead of promoting
   *  this frame. */
  onScreenDrawer?(mode: ScreenPresentationMode, surface?: string, independent?: boolean, size?: ScreenDrawerSize): void;
  /** CAD-1177: invoke one declared tool alias under the operator's own
   *  session + this mount's server-bound action context. Absent (no tool
   *  context was minted, e.g. a v1/read-only screen) the `tool` op closes. */
  invoke?(alias: string, input: Record<string, unknown>, requestId: string, scope?:import("./screenProtocol").GenerationScope): Promise<ActionResult>;
}
/** Most calls one frame may have outstanding; one more closes it. */
export const CALL_QUEUE_MAX = 8;
/** A reply's data is at most this many bytes of JSON. */
const REPLY_BYTES_MAX = 64 * 1024;
const RECORD_REPLY_BYTES_MAX = 1048576;
const RECORD_REPLY_WIRE_BYTES_MAX = RECORD_REPLY_BYTES_MAX + 1024;
const jsonBytes = (value: unknown): number => new TextEncoder().encode(JSON.stringify(value)).byteLength;

declare const __SHELL_VERSION__: string | undefined;
/** The shell's own version, injected by the Vite build from `ui/package.json`.
 *  The test runner has no define, so the package version's default stands. */
export const shellVersion: string = typeof __SHELL_VERSION__ === "string" ? __SHELL_VERSION__ : "0.1.0";

/** Order two dotted numeric versions. Both sides here are validated shell
 *  versions (at most three numeric parts), so a missing part reads as 0. */
export function compareShellVersions(left: string, right: string): number {
  const a = left.split(".").map(Number);
  const b = right.split(".").map(Number);
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const diff = (a[i] ?? 0) - (b[i] ?? 0);
    if (diff !== 0) return diff > 0 ? 1 : -1;
  }
  return 0;
}

export type MountReceipt = { mount: string; bridge_nonce: string; generation: number; tag: string;
  /** The oldest shell the screen's contract supports (three numeric parts,
   *  e.g. `1.2.3`). The shell refuses to mount below it. */
  min_shell?: string;
  /** CAD-1177: the mount's server-bound action context token, held by the
   *  host only (never sent to the frame) to prove a `tool` call belongs to
   *  this mount. `null`/absent for a read-only screen. */
  action_token?: string | null;
  /** CAD-1177: the tool aliases the screen declared, named by the daemon
   *  only when a tool context was minted. Advertising an alias grants
   *  nothing: every invoke still re-proves binding, approval and operator. */
  tools?: string[];
  /** CAD-1365: host-held media metadata for a `screen-media.v1` package
   *  — the daemon-minted record of the media permissions the screen
   *  DECLARED (`media.read` / `media.publish`, either or both, never
   *  wider). Declaration metadata only: dispatch still requires the
   *  mount's action token and the child's `screen-media.v1` opt-in, and
   *  the daemon re-proves the declaration natively on every call. */
  media?: { contract: "screen-media.v1"; may: MediaVerb[] };
  host_contract?: typeof SCREEN_RECORDS_V1;
  operations?: string[];
  navigation?: ScreenNavigation | null;
  display_scope_key?: string | null;
  display_session_key?: string;
  install_id?: string; context_id?: string | null; digest?: string;
  remote_images?: string[] };
export function parseNavigation(value: unknown): ScreenNavigation | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const nav = value as ScreenNavigation;
  const reserved = new Set(["app", "install", "install_id", "context", "context_id", "ctx", "actor", "identity", "auth", "token", "key", "session", "session_key", "origin", "project", "workspace", "conversation", "thread", "message", "view", "appview", "record", "record_id", "segment", "segment_id", "screen", "contract-preview", "nonce", "generation", "digest"]);
  if (Object.keys(nav).sort().join() !== "default_section,section_query,sections" || typeof nav.section_query !== "string" || !/^[a-z][a-z0-9_-]{0,31}$/.test(nav.section_query) || reserved.has(nav.section_query) || !Array.isArray(nav.sections) || nav.sections.length < 1 || nav.sections.length > 8 || new Set(nav.sections.map(s => s?.id)).size !== nav.sections.length || !nav.sections.every(s => s && Object.keys(s).sort().join() === "id,label" && typeof s.id === "string" && /^[a-z0-9][a-z0-9-]{0,31}$/.test(s.id) && typeof s.label === "string" && s.label.trim().length > 0 && s.label.length <= 40) || !nav.sections.some(s => s.id === nav.default_section)) return null;
  return nav;
}
export function parseMount(value: unknown, tag: string): MountReceipt | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const v = value as Record<string, unknown>;
  const keySet = Object.keys(v);
  const keysOk = ["bridge_nonce", "generation", "mount", "tag"].every(k => keySet.includes(k)) &&
    keySet.every(k => ["bridge_nonce", "generation", "mount", "tag", "min_shell", "action_token", "tools", "media", "host_contract", "operations", "navigation", "display_scope_key", "display_session_key", "install_id", "context_id", "digest", "remote_images"].includes(k));
  const minShell = v.min_shell;
  const minShellOk = minShell === undefined || (typeof minShell === "string" && /^\d+\.\d+\.\d+$/.test(minShell));
  const tools = v.tools;
  const toolsOk = tools === undefined || (Array.isArray(tools) && tools.length <= 32 &&
    new Set(tools).size === tools.length && tools.every(a => typeof a === "string" && TOOL_ALIAS.test(a)));
  // `media` is a closed object when present: exactly
  // {contract:"screen-media.v1", may:[1..2 distinct declared verbs]}.
  const media = v.media;
  const mediaOk = media === undefined || (typeof media === "object" && media !== null && !Array.isArray(media) &&
    Object.keys(media as Record<string, unknown>).sort().join() === "contract,may" &&
    (media as Record<string, unknown>).contract === SCREEN_MEDIA_V1 &&
    Array.isArray((media as Record<string, unknown>).may) &&
    ((media as Record<string, unknown>).may as unknown[]).length >= 1 &&
    ((media as Record<string, unknown>).may as unknown[]).length <= 3 &&
    new Set((media as Record<string, unknown>).may as unknown[]).size === ((media as Record<string, unknown>).may as unknown[]).length &&
    ((media as Record<string, unknown>).may as unknown[]).every(m => isMediaVerb(m as string)));
  const action = v.action_token;
  const actionOk = action === null || action === undefined ||
    (typeof action === "string" && /^[a-f0-9]{64}$/.test(action) && !v.mount?.toString().endsWith(action));
  const records = v.host_contract === SCREEN_RECORDS_V1;
  const recordsKeys = ["operations", "navigation", "display_scope_key", "display_session_key", "install_id", "context_id", "digest", "remote_images"];
  const recordsOk = records ?
    media === undefined && typeof action === "string" && Array.isArray(tools) && tools.length === 0 && Array.isArray(v.operations) && v.operations.length > 0 && v.operations.length <= 64 && new Set(v.operations).size === v.operations.length && v.operations.every(op => typeof op === "string" && RECORD_OPERATIONS.includes(op)) &&
    (v.navigation === null || parseNavigation(v.navigation) !== null) && typeof v.display_session_key === "string" && /^[a-f0-9]{64}$/.test(v.display_session_key) &&
    typeof v.install_id === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(v.install_id) && typeof v.context_id === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(v.context_id) && typeof v.digest === "string" && /^sha256:[a-f0-9]{64}$/.test(v.digest) && typeof v.display_scope_key === "string" && /^[a-f0-9]{64}$/.test(v.display_scope_key) &&
    Array.isArray(v.remote_images) && v.remote_images.length <= 1 && v.remote_images.every(x => x === "instagram-cdn")
    : v.host_contract === undefined && recordsKeys.every(k => v[k] === undefined);
  if (!keysOk || !minShellOk || !toolsOk || !mediaOk || !recordsOk ||
      typeof v.mount !== "string" || !/^\/api\/app-screen\/[a-f0-9]{64}$/.test(v.mount) ||
      typeof v.bridge_nonce !== "string" || !/^[a-f0-9]{64}$/.test(v.bridge_nonce) ||
      v.mount.endsWith(v.bridge_nonce) || v.tag !== tag || !actionOk ||
      typeof v.generation !== "number" || !Number.isSafeInteger(v.generation) || v.generation < 0) return null;
  return v as MountReceipt;
}

/**
 * The closed receive side every mounted frame shares (CAD-1006): source,
 * opaque-origin, single-init, tag/nonce/generation and the closed child
 * vocabulary are checked here and nowhere else. A subclass decides only
 * what a child's `ready` opts into and what is pushed after it. Teardown
 * precedes replacement.
 */
export abstract class FrameChannel {
  protected port: MessagePort | null = null;
  protected ready = false;
  protected closed = false;
  private initialized = false;
  private loads = 0;
  constructor(protected source: Window, protected receipt: MountReceipt,
    protected removeFrame: () => void, protected failed: () => void, private onReady: () => void) {}
  /** The child's one `ready`: record its opt-in, or refuse it (return false closes). */
  protected abstract accept(ready: Extract<ChildToHost, { op: "ready" }>): boolean;
  /** Send the negotiated shape (called once after `ready`). */
  protected abstract send(): void;
  /** A screen.v2 `asset` request (only after `ready`). A frame kind that has
   *  no image channel refuses it: the default closes the port. */
  protected asset(_ref: string): void { this.close(true); }
  /** screen.v2 `call` / `slot` / `tool` ops: a frame kind with no action
   *  host closes. */
  protected call(_request: CallRequest): void { this.close(true); }
  protected slot(_request: SlotRequest): void { this.close(true); }
  protected tool(_request: ToolRequest): void { this.close(true); }
  protected recordsRights(): readonly string[] | undefined { return undefined; }
  /** CAD-1777 — whether this frame may use the host's local drawer UI verbs,
   *  and which of them. Only the screen channel can ever say yes; a
   *  primary admits both, a drawer frame only `close`. */
  protected drawerCapable(): boolean | readonly ScreenDrawerVerb[] { return false; }
  /** Opaque state is ignored unless this frame kind explicitly understands it. */
  protected state(_data: string): void {}
  receive(event: Pick<MessageEvent, "origin" | "source" | "data" | "ports">): void {
    if (this.closed || event.source !== this.source) return;
    if (event.origin !== "null") {
      event.ports.forEach(p => p.close()); this.close(true); return;
    }
    const init = parseInit(event.data);
    if (!init || init.tag !== this.receipt.tag || init.bridge_nonce !== this.receipt.bridge_nonce ||
        init.generation !== this.receipt.generation || event.ports.length !== 1 || this.initialized) {
      event.ports.forEach(p => p.close()); this.close(true); return;
    }
    this.initialized = true;
    this.port = event.ports[0];
    this.port.onmessage = event => {
      if (this.closed) return;
      const child = parseChild(event.data, this.recordsRights(), this.drawerCapable());
      if (!child || child.op === "state" && !this.ready) {
        console.warn("screen frame closed: protocol violation (unparsable or early message)");
        this.close(true); return;
      }
      if (child.op === "ready") {
        if (this.ready || !this.accept(child)) { this.close(true); return; }
        this.ready = true;
        this.send();
        if (!this.closed) this.onReady();
      }
      if (child.op === "asset") {
        if (!this.ready) { this.close(true); return; }
        this.asset(child.ref);
      }
      if (child.op === "call" || child.op === "slot" || child.op === "tool") {
        if (!this.ready) { this.close(true); return; }
        if (child.op === "call") this.call(child);
        else if (child.op === "slot") this.slot(child);
        else this.tool(child);
      }
      if (child.op === "state") this.state(child.data);
      // No local draft content is persisted.
    };
    this.port.onmessageerror = () => this.close(true);
    this.port.start();
  }
  load(): void { if (++this.loads > 1) this.close(true); }
  close(report = false): void {
    if (this.closed) return;
    this.closed = true;
    if (this.port) { this.port.onmessage = null; this.port.onmessageerror = null; this.port.close(); }
    this.port = null;
    this.removeFrame();
    if (report) this.failed();
  }
}

/** Each instance owns one frame generation. Teardown precedes replacement. */
/** Verbs whose planner runs against the consumed mount receipt: the original download and
 *  the social publish relays send the host-held action token, never a frame-supplied credential. */
const RECEIPT_TOKEN_VERBS: readonly string[] = [ORIGINAL_DOWNLOAD_VERB, "publish.draft.confirm", "publish.draft.schedule"];

export class ScreenChannel extends FrameChannel {
  /** Set once, by the child's `ready`: the PUSH shape it negotiated. */
  private shape: Shape = "v1";
  /** The image refs of the last PUSH actually sent — the only refs the
   *  child may request. */
  private refs = new Set<string>();
  private dynamicDraftRefs = new Set<string>();
  /** Outstanding refs (queued or loading), deduplicated. */
  private assets = new Set<string>();
  private queue: string[] = [];
  private loading = 0;
  constructor(source: Window, receipt: MountReceipt, private projection: ScreenPush,
    removeFrame: () => void, failed: () => void, onReady: () => void,
    private loadAsset: AssetLoader = async () => null, private actions?: ScreenActions,
    private clock?: () => number, private onUnsaved?: (unsaved: boolean) => void) {
    super(source, receipt, removeFrame, failed, onReady);
  }
  private records = false;
  private crmLocale = false;
  private wantsPresentation = false;
  private wantsSurfaces = false;
  /** CAD-1777 — the host-derived surface role of this mount. The outlet
   *  sets it from its own mount request (a drawer only exists behind a
   *  private parent token); the ready opt-in and call args cannot choose
   *  it. */
  surfaceRole: ScreenSurfaceRole = "primary";
  private surfaceState: { role: ScreenSurfaceRole; modal: boolean } | null = null;
  private surfaceSent: string | null = null;
  /** CAD-1615 — tell the child which surface its frame is in (legacy). The
   *  host owns the geometry; the child draws its surface there. */
  present(mode: ScreenPresentationMode): void {
    if (this.closed || !this.ready || !this.port || !this.wantsPresentation) return;
    const message: ScreenPresentation = { v: 1, op: "presentation", mode };
    this.port.postMessage(message);
  }
  /** CAD-1777 — the host-owned role/modal state for a `screen-surfaces.v1`
   *  child. `send()` posts it before the first projection, so a primary
   *  never flashes list content when it is meant to draw a drawer; every
   *  later change is posted once. */
  surface(role: ScreenSurfaceRole, modal: boolean, drawerSurface?: string): void {
    if (this.surfaceState?.role === role && this.surfaceState.modal === modal && this.drawerSurface === drawerSurface) return;
    this.surfaceState = { role, modal };
    this.drawerSurface = drawerSurface;
    this.postSurface();
  }
  /** CAD-1777 — the closed surface idiom a mounted drawer child draws. */
  private drawerSurface: string | undefined;
  private postSurface(): void {
    if (this.closed || !this.ready || !this.port || !this.wantsSurfaces) return;
    this.surfaceState ??= { role: this.surfaceRole, modal: false };
    const named = this.surfaceState.role === "drawer" && this.drawerSurface ? this.drawerSurface : undefined;
    const key = `${this.surfaceState.role}:${this.surfaceState.modal ? 1 : 0}:${named ?? ""}`;
    if (this.surfaceSent === key) return;
    this.surfaceSent = key;
    const message: ScreenSurface = { v: 1, op: "surface", role: this.surfaceState.role, modal: this.surfaceState.modal, ...(named ? { surface: named } : {}) };
    this.port.postMessage(message);
  }
  protected state(data: string): void {
    // Any records-contract screen may publish the hint; the payload's own
    // `contract` field is the declaration (CAD-1543), never the app name.
    if (!this.ready || this.closed || !this.records || data.length > 128 ||
        new TextEncoder().encode(data).byteLength > 128) return;
    // This closed boolean is a warning hint, never navigation/effect authority.
    // Unknown or malformed opaque state must not clear an existing warning.
    try {
      const value: unknown = JSON.parse(data);
      if (!value || typeof value !== "object" || Array.isArray(value)) return;
      const state = value as Record<string, unknown>;
      if (Object.keys(state).sort().join() !== "contract,unsaved" ||
          state.contract !== "crm-leave.v1" || typeof state.unsaved !== "boolean") return;
      this.onUnsaved?.(state.unsaved);
    } catch { /* Ignore other opaque state, without changing the warning. */ }
  }
  private csvBusy = false;
  private csvSlot: string | null = null;
  protected recordsRights(): readonly string[] | undefined { return this.records ? this.receipt.operations ?? [] : undefined; }
  /** CAD-1777 / CAD-1932 — the host-local presentation capability of THIS
   *  live mount: any screen.v2 frame (records or not) that negotiated
   *  `screen-presentation.v1` or `screen-surfaces.v1`, has an admitted
   *  action host, a live action context and a live shell drawer callback.
   *  A read-only viewer's shell withholds the callback, so it never
   *  inherits geometry authority from a receipt. A primary admits both
   *  verbs; a drawer frame admits only its own close. */
  private drawerVerbs(): readonly ScreenDrawerVerb[] | null {
    if (this.shape !== "v2" || !(this.wantsPresentation || this.wantsSurfaces) ||
        !this.actions?.onScreenDrawer || !this.receipt.action_token) return null;
    return this.surfaceRole === "drawer" ? [SCREEN_DRAWER_CLOSE] : SCREEN_DRAWER_VERBS;
  }
  protected drawerCapable(): readonly ScreenDrawerVerb[] | false { return this.drawerVerbs() ?? false; }
  /** One validated host-local presentation request. It is never dispatched
   *  to a records service or an app action, never spends and never widens
   *  rights; malformed arguments are refused in place so the frame (and its
   *  draft) survives. A verb outside this mount's admitted subset (a drawer
   *  frame's open) closes, as on the records parser. */
  private drawerCall(request: CallRequest & { verb: ScreenDrawerVerb }): void {
    const onDrawer = this.actions?.onScreenDrawer;
    if (!this.drawerVerbs()?.includes(request.verb) || !onDrawer) { this.close(true); return; }
    const independent = this.wantsSurfaces;
    const reply = (message: ReplyMessage) => { if (!this.closed) this.port?.postMessage(message); };
    const parsed = parseScreenDrawerCall(request.verb, request.args);
    if (!parsed) {
      reply({ v: 2, op: "reply", id: request.id, ok: false, refusal: { code: "bad_args", text: "That request is not valid." } });
      return;
    }
    if (parsed.mode === "outlet") {
      onDrawer("outlet", undefined, independent);
      reply({ v: 2, op: "reply", id: request.id, ok: true, data: { mode: "outlet" } });
      return;
    }
    onDrawer("drawer", parsed.surface, independent, parsed.size);
    reply({ v: 2, op: "reply", id: request.id, ok: true, data: { mode: "drawer", surface: parsed.surface } });
  }
  private calls = 0;
  /** In-flight call continuations. Close aborts them; resume does not restart them. */
  private callRetire: AbortController[] = [];
  private slots: SlotController | null = null;
  /** CAD-1365: a media verb dispatches only for a v2 child that opted
   *  into `screen-media.v1`, on an operator-scoped mount (nonempty
   *  projected actions) holding a live action token, whose minted
   *  receipt declares this exact verb in `media.may`. Every check is
   *  host-held metadata — the frame's own fields never widen it, and a
   *  denied verb closes before any dispatch. */
  private mediaAdmits(verb: MediaVerb): boolean {
    return !this.records && this.shape === "v2" && this.wantsMedia && !!this.actions &&
      !!this.projection.actions?.length && !!this.receipt.action_token &&
      this.receipt.media?.contract === SCREEN_MEDIA_V1 &&
      this.receipt.media.may.includes(verb);
  }
  /** CAD-1365: the original download is qualified only on the same media
   *  gates as `media.read`, plus the real planner. Support is never permission. */
  private originalDownloadAdmits(): boolean {
    return this.mediaAdmits("media.read") && !!this.actions?.planner;
  }
  /** Only a v2 child with an action host; the verb set was closed by the parser. */
  protected call(request: CallRequest): void {
    const actions = this.actions;
    if (this.crmLocale && Object.hasOwn(request.args, "presentation_locale")) { this.close(true); return; }
    if (this.shape !== "v2" || !actions || this.calls >= CALL_QUEUE_MAX) { this.close(true); return; }
    // CAD-1777 / CAD-1932 — a drawer request is host-local UI, never a
    // records operation or an app action: a capable frame (records or not)
    // is answered here and never reaches the services. A non-records
    // `screen-presentation.v1` peer without that capability keeps its
    // CAD-1615 path below; any other non-records frame (no opt-in, or a
    // surfaces frame without a live action context or shell callback) is
    // refused in place.
    if (isScreenDrawerVerb(request.verb)) {
      if (this.records || this.drawerVerbs()) { this.drawerCall(request as CallRequest & { verb: ScreenDrawerVerb }); return; }
      if (!this.wantsPresentation || this.wantsSurfaces) {
        this.port?.postMessage({ v: 2, op: "reply", id: request.id, ok: false, refusal: { code: "unavailable", text: "This app cannot open a board drawer here." } });
        return;
      }
    }
    const csv = this.records && request.verb === "records.csv.preview";
    if (csv && (this.csvBusy || this.csvSlot)) { this.close(true); return; }
    if (csv) this.csvBusy = true;
    if (this.records ? !this.receipt.operations?.includes(request.verb) || !(RECORD_CALL_VERBS as readonly string[]).includes(request.verb) : !(CALL_VERBS as readonly string[]).includes(request.verb)) { this.close(true); return; }
    if (isMediaVerb(request.verb) && !this.mediaAdmits(request.verb)) { this.close(true); return; }
    this.calls++;
    const retire = new AbortController();
    this.callRetire.push(retire);
    const reply = (message: ReplyMessage) => {
      if (this.closed) return;
      if (this.records && jsonBytes(message) > RECORD_REPLY_WIRE_BYTES_MAX) {
        this.port?.postMessage({ v: 2, op: "reply", id: request.id, ok: false, refusal: { code: "too_large", text: "That is too large to show." } });
      } else this.port?.postMessage(message);
    };
    void actions.call(request.verb, request.args, { showLink: url => actions.onLink(url), actionToken: this.receipt.action_token ?? undefined, signal: retire.signal, openScreenDrawer: actions.onScreenDrawer ? (surface: string) => { actions.onScreenDrawer?.("drawer", surface); return true; } : undefined, closeScreenDrawer: actions.onScreenDrawer ? () => { actions.onScreenDrawer?.("outlet"); return true; } : undefined })
      .catch((): ActionResult => ({ ok: false, refusal: { code: "failed", text: "That didn't work." } }))
      .then(result => {
        this.calls--;
        if (csv) this.csvBusy = false;
        if (this.closed) return;
        if (!result.ok) { reply({ v: 2, op: "reply", id: request.id, ok: false, refusal: result.refusal }); return; }
        const previewBytes = request.verb === "media.read" && request.args.kind === "preview" ? 2 * 1024 * 1024 * 4 / 3 + 1024 : 0;
        const size = jsonBytes(result.data ?? null);
        if (size > Math.max(this.records ? RECORD_REPLY_BYTES_MAX : REPLY_BYTES_MAX, previewBytes)) reply({ v: 2, op: "reply", id: request.id, ok: false, refusal: { code: "too_large", text: "That is too large to show." } });
        else {
          const admitted = this.rememberDraftAssetRefs(result.data,request.args);
          // CAD-1921: name the draft-image refs this reply made requestable, so the app can ask at
          // once instead of waiting for the next PUSH. Older receivers ignore extra reply keys; a
          // records frame (whose bridge is exact-key) never gets it, and an older host never sends it.
          reply({ v: 2, op: "reply", id: request.id, ok: true, data: result.data ?? null, ...(admitted.length && !this.records ? { asset_refs: admitted } : {}) });
        }
      })
      .finally(() => {
        const index = this.callRetire.indexOf(retire);
        if (index >= 0) this.callRetire.splice(index, 1);
      });
  }
/** Only a v2 child with an action host. At most one live slot per frame. */
  protected slot(request: SlotRequest): void {
    const actions = this.actions;
    if (this.crmLocale && Object.hasOwn(request.args, "presentation_locale")) { this.close(true); return; }
    if (this.shape !== "v2" || !actions || (this.records ? !this.receipt.operations?.includes(request.verb) || !(RECORD_SLOT_VERBS as readonly string[]).includes(request.verb) : !(SLOT_VERBS as readonly string[]).includes(request.verb))) { this.close(true); return; }
    const csv = this.records && (request.verb === "records.csv.import" || request.verb === "records.csv.confirm");
    if (csv && (this.csvBusy || this.csvSlot !== null && this.csvSlot !== request.id)) { this.close(true); return; }
    if (csv) this.csvSlot = request.id;
    if (request.verb === ORIGINAL_DOWNLOAD_VERB && !this.originalDownloadAdmits()) { this.close(true); return; }
    // The action token comes only from the consumed mount receipt, never from the frame's args.
    const token = this.receipt.action_token ?? undefined;
    const planner: Planner = (verb, args) => actions.planner(verb, args, RECEIPT_TOKEN_VERBS.includes(verb)
      ? { actionToken: token, isCurrent: () => !this.closed && this.receipt.action_token === token } : undefined);
    this.slots ??= new SlotController(message => {
      if (message.op === "slot-state" && message.state !== "pending" && this.csvSlot === message.id) this.csvSlot = null;
      if (this.closed) return;
      if (this.records && (jsonBytes(message) > RECORD_REPLY_WIRE_BYTES_MAX || "data" in message && jsonBytes(message.data ?? null) > RECORD_REPLY_BYTES_MAX)) {
        this.port?.postMessage({ v: 2, op: "slot-state", id: message.id, state: "refused", refusal: { code: "too_large", text: "That receipt is too large to show." } });
      } else this.port?.postMessage(message);
    }, view => actions.onSlot(view), planner, this.clock);
    void this.slots.request(request);
  }
  /** Only a v2 child that opted into `screen-actions.v1` AND whose host
   *  holds a live mount action context. The alias is dispatched by the
   *  daemon against the closed declared set — the host never trusts the
   *  frame's choice. One outstanding tool call at a time. */
  private wantsTools = false;
  private wantsMedia = false;
  private toolBusy = false;
  protected tool(request: ToolRequest): void {
    const actions = this.actions;
    if (this.shape !== "v2" || !this.wantsTools || !actions?.invoke || this.toolBusy) {
      this.close(true); return;
    }
    this.toolBusy = true;
    const reply = (message: ReplyMessage) => { if (!this.closed) this.port?.postMessage(message); };
    void actions.invoke(request.alias, request.input, request.request_id, request.generation_scope)
      .catch((): ActionResult => ({ ok: false, refusal: { code: "failed", text: "That didn't work." } }))
      .then(result => {
        this.toolBusy = false;
        if (!result.ok) { reply({ v: 2, op: "tool-reply", id: request.id, ok: false, refusal: result.refusal }); return; }
        const size = new TextEncoder().encode(JSON.stringify(result.data ?? null)).byteLength;
        if (size > REPLY_BYTES_MAX) reply({ v: 2, op: "tool-reply", id: request.id, ok: false, refusal: { code: "too_large", text: "That is too large to show." } });
        else reply({ v: 2, op: "tool-reply", id: request.id, ok: true, data: result.data ?? null });
      });
  }
  /** A press on the host-drawn button (the board component calls this). */
  tapSlot(token: string, trusted: boolean, pressedAt: number, input?: string): boolean {
    return !this.closed && (this.slots?.tap(token, trusted, pressedAt, input) ?? false);
  }
  /** SOC-32: admit one host-minted ref for the asset channel after a host
   *  upload slot. The ref grammar is closed; the loader still re-proves the
   *  live action token and context before returning any bytes. */
  allowAssetRef(ref: string): void {
    if (this.closed || this.shape !== "v2" || !ASSET_REF.test(ref) || this.dynamicDraftRefs.size >= 128) return;
    this.dynamicDraftRefs.add(ref);
    this.refs.add(ref);
  }
  /** The person closed the host-drawn confirm card. */
  cancelSlot(token: string): void {
    if (!this.closed) this.slots?.cancel(token);
  }
  close(report = false): void {
    for (const retire of this.callRetire) retire.abort();
    this.callRetire = [];
    this.slots?.close();
    super.close(report);
  }
  protected accept(ready: Extract<ChildToHost, { op: "ready" }>): boolean {
    // Only the opt-ins a workspace screen can honour; the chat opt-in closes it.
    const shape = shapeOf(ready.accepts);
    if (!shape) return false;
    const accepts: readonly string[] = ready.accepts ?? [];
    const records = accepts.includes(SCREEN_RECORDS_V1);
    if (records !== (this.receipt.host_contract === SCREEN_RECORDS_V1) || records && (shape !== "v2" || !accepts.includes(SCREEN_V2) || wantsActions(ready.accepts) || !this.receipt.action_token || !this.actions)) return false;
    this.records = records;
    this.crmLocale = records && accepts.includes(CRM_LOCALE_V1);
    this.shape = shape;
    // Record the tool/action opt-in: the child may then send `tool` ops,
    // which only run when the host holds a live mount action context.
    this.wantsTools = wantsActions(ready.accepts);
    // Record the media opt-in: the child may then send media call verbs,
    // which still dispatch only on the receipt's declared `may` (a
   //  tool-less media mount's receipt carries no action token and its
    // calls refuse in `call`).
    this.wantsMedia = wantsMedia(ready.accepts);
    // Record the presentation opt-in: only a child that advertises it may
    // be told which surface its frame is in.
    this.wantsPresentation = accepts.includes(SCREEN_PRESENTATION_V1);
    // CAD-1777 — the newer host-owned surface push (role + modal). It
    // replaces the presentation token for packages that can draw as a
    // primary or a drawer; a frame that does not advertise it never sees
    // the message.
    this.wantsSurfaces = accepts.includes(SCREEN_SURFACES_V1);
    return true;
  }
  /** Only a v2 child, for a ref the last PUSH carried or a call reply just
   *  returned (CAD-1324); anything else closes. */
  protected asset(ref: string): void {
    if (this.shape !== "v2" || !(this.refs.has(ref) || this.dynamicDraftRefs.has(ref)) ||
        (!this.assets.has(ref) && this.assets.size >= ASSET_QUEUE_MAX)) {
      // The ref class only: a full ref names a draft, and nothing else about the request is logged.
      const kind = ref.startsWith("draft-image:") ? "draft-image" : this.refs.has(ref) ? "pushed" : "unknown";
      console.warn(`screen frame closed: asset request refused (${this.shape === "v2" ? kind : "not v2"})`);
      this.close(true); return;
    }
    if (!this.assets.has(ref)) { this.assets.add(ref); this.queue.push(ref); this.pump(); }
  }
  update(projection: ScreenPush): void {
    if (this.closed) return;
    if (projection.install_id !== this.projection.install_id || projection.digest !== this.projection.digest ||
        projection.tag !== this.projection.tag || projection.context_id !== this.projection.context_id) {
      this.close(true); return;
    }
    this.projection = projection;
    if (this.ready) this.send();
  }
  /** Advertise the mount's declared tool aliases beside the closed verbs,
   *  only to an operator-scoped (non-empty `actions`) v2 child that opted
   *  into tools on a mount that holds an action context. Advertising grants
   *  nothing: the invoke path re-proves everything per call. */
  private withToolAliases(projection: ScreenPush): ScreenPush {
    const aliases = this.receipt.tools;
    if (this.shape !== "v2" || !this.wantsTools || !this.actions?.invoke || !this.receipt.action_token ||
        !aliases?.length || !projection.actions?.length) return projection;
    return { ...projection, actions: [...projection.actions, ...aliases.filter(a => !projection.actions!.includes(a))] };
  }
  /** Advertise the media verbs this mount was minted with — only to a
   *  media-opted-in v2 child on an eligible mount (the same gates
   *  `mediaAdmits` enforces at dispatch). Media verbs are never in the
   *  generic `ACTION_VERBS` set, so a legacy or non-media frame never
   *  sees them. */
  private withMediaVerbs(projection: ScreenPush): ScreenPush {
    const may = this.receipt.media?.may ?? [];
    const advertised: string[] = may.filter(verb => this.mediaAdmits(verb) && !projection.actions?.includes(verb));
    if (this.originalDownloadAdmits() && projection.actions?.length && !projection.actions.includes(ORIGINAL_DOWNLOAD_VERB))
      advertised.push(ORIGINAL_DOWNLOAD_VERB);
    if (!advertised.length || !projection.actions?.length) return projection;
    return { ...projection, actions: [...projection.actions, ...advertised] };
  }
  /** Send the negotiated shape, or close when it cannot be sent within bounds. */
  protected send(): void {
    // DSN-1: `run_receipts` is a host-derived marker, never forwarded. Any
    // value on the incoming projection is discarded first; it is set only
    // for a v2 child whose mount holds a real planner AND whose projected
    // action list already carries `run.start` (a no-action host or a
    // frame that merely forges the marker gets none). Granting nothing:
    // the slot path still validates, plans and confirms natively.
    // CAD-1447: the marker also requires the child's `screen-media.v1` opt-in —
    // the released `screen-actions.v1` SDK rejects the unknown root field.
    const projection: ScreenPush = this.records ? { ...this.projection, actions: [...(this.receipt.operations ?? [])] } : { ...this.projection };
    // CAD-1777 — the host-local presentation verbs are advertised beside
    // the receipt's records operations, and only to a frame that has both
    // the negotiated capability and the live shell drawer. A primary gets
    // both; a drawer frame gets only its own close. They are UI, not
    // records rights: a non-capable peer keeps exactly its old shape.
    const drawerVerbs = this.drawerVerbs();
    if (drawerVerbs) {
      const advertised = projection.actions ?? [];
      projection.actions = [...advertised, ...drawerVerbs.filter(verb => !advertised.includes(verb))];
    }
    // Presentation negotiation grants no actions. Older/other peers retain their exact shape.
    if (!this.crmLocale || !["en", "zh-TW"].includes(projection.presentation_locale ?? "")) delete projection.presentation_locale;
    delete projection.run_receipts;
    if (!this.records && this.shape === "v2" && this.wantsMedia && this.actions?.planner && projection.actions?.includes("run.start")) {
      projection.run_receipts = 1;
    }
    // CAD-1365: `original_downloads` is host-derived the same way, and only for
    // a v2 child whose mount may read media. It grants nothing by itself.
    delete projection.original_downloads;
    delete projection.media_previews;
    delete projection.media_chat_references;
    if (this.shape === "v2" && this.originalDownloadAdmits() && projection.actions?.length) projection.original_downloads = 1;
    if (this.shape === "v2" && this.mediaAdmits("media.read")) projection.media_previews = 1;
    if (this.shape === "v2" && this.mediaAdmits("media.chat.attach")) projection.media_chat_references = 1;
    const push = shapeFor(this.records ? projection : this.withMediaVerbs(this.withToolAliases(projection)), this.shape);
    if (!push) { this.close(true); return; }
    this.refs = this.shape === "v2" ? pushedAssetRefs(push) : new Set();
    if(this.shape==="v2") for(const ref of this.dynamicDraftRefs) this.refs.add(ref);
    // CAD-1777 — the host-owned surface arrives BEFORE the first projection
    // (and before every later one, deduplicated), so a primary never draws
    // list content when it is meant to draw the drawer.
    this.postSurface();
    this.port?.postMessage(push);
  }
  private rememberDraftAssetRefs(data:unknown,args:Record<string,unknown>):string[] {
    const admitted:string[]=[];
    const alias=args.alias;
    if(typeof alias!=="string"||!TOOL_ALIAS.test(alias)) return admitted;
    const values=Array.isArray((data as Record<string,unknown>|null)?.drafts)?(data as {drafts:unknown[]}).drafts:[data];
    for(const value of values){
      if(!value||typeof value!=="object"||Array.isArray(value)) continue;
      const row=value as Record<string,unknown>; const id=row.draft_id;
      if(typeof id!=="string"||!/^[A-Za-z0-9_-]{1,128}$/.test(id)) continue;
      const prefix=`draft-image:${id}:`;
      for(const ref of this.dynamicDraftRefs) if(ref.startsWith(prefix)) this.dynamicDraftRefs.delete(ref);
      if(typeof row.asset_id==="string"&&row.asset_id.length>0){
        const ref=`draft-image:${id}:${alias}`;
        if(ASSET_REF.test(ref)&&this.dynamicDraftRefs.size<128){this.dynamicDraftRefs.add(ref);admitted.push(ref);}
      }
    }
    return admitted;
  }
  private pump(): void {
    while (!this.closed && this.loading < ASSET_PARALLEL && this.queue.length) {
      const ref = this.queue.shift()!;
      this.loading++;
      void this.serveAsset(ref).finally(() => { this.loading--; this.pump(); });
    }
  }
  /** One reply per request, and none when the ref cannot be loaded now or
   *  the result breaks the image contract (the frame keeps a placeholder). */
  private async serveAsset(ref: string): Promise<void> {
    let dataUrl: string | null = null;
    try { dataUrl = await this.loadAsset(ref); } catch { dataUrl = null; }
    this.assets.delete(ref);
    if (this.closed || typeof dataUrl !== "string" || dataUrl.length > ASSET_BYTES_MAX || !ASSET_DATA_URL.test(dataUrl)) return;
    const reply: AssetReply = { v: 2, op: "asset", ref, data_url: dataUrl };
    this.port?.postMessage(reply);
  }
}
