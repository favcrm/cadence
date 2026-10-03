import { ASSET_BYTES_MAX, ASSET_DATA_URL, parseChild, parseInit, pushedAssetRefs, shapeFor, shapeOf,
  type ActionResult, type AssetReply, type CallRequest, type CallVerb, type ChildToHost, type ReplyMessage, type ScreenPush,
  type Shape, type SlotRequest } from "./screenProtocol";
import { SlotController, type Planner, type SlotView } from "./screenSlot";

/** Resolves one pushed asset ref to a downscaled `data:` URL, or `null`
 *  when it cannot be loaded now. Supplied by the board; never by the frame. */
export type AssetLoader = (ref: string) => Promise<string | null>;
/** Most asset requests one frame may have outstanding (the package's
 *  per-mount budget); one more closes it. At most ASSET_PARALLEL load at once. */
export const ASSET_QUEUE_MAX = 256;
const ASSET_PARALLEL = 4;

/** What the board lends one mounted frame for CAD-1123 HP3 actions. Absent
 *  (a non-operator view, a chat frame) the `call` and `slot` ops close the port. */
export interface ScreenActions {
  /** Run a non-spending verb as the board's operator session. */
  call(verb: CallVerb, args: Record<string, unknown>, ui: { showLink(url: string): void }): Promise<ActionResult>;
  /** Validate a spend verb, fetch what the host must check, and name the work. */
  planner: Planner;
  /** The host-drawn slot to show (or hide, `null`). */
  onSlot(view: SlotView | null): void;
  onLink(url: string): void;
}
/** Most calls one frame may have outstanding; one more closes it. */
export const CALL_QUEUE_MAX = 8;
/** A reply's data is at most this many bytes of JSON. */
const REPLY_BYTES_MAX = 64 * 1024;

export type MountReceipt = { mount: string; bridge_nonce: string; generation: number; tag: string };
export function parseMount(value: unknown, tag: string): MountReceipt | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const v = value as Record<string, unknown>;
  if (Object.keys(v).sort().join() !== "bridge_nonce,generation,mount,tag" ||
      typeof v.mount !== "string" || !/^\/api\/app-screen\/[a-f0-9]{64}$/.test(v.mount) ||
      typeof v.bridge_nonce !== "string" || !/^[a-f0-9]{64}$/.test(v.bridge_nonce) ||
      v.mount.endsWith(v.bridge_nonce) || v.tag !== tag ||
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
  /** screen.v2 `call` / `slot` ops: a frame kind with no action host closes. */
  protected call(_request: CallRequest): void { this.close(true); }
  protected slot(_request: SlotRequest): void { this.close(true); }
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
      const child = parseChild(event.data);
      if (!child || child.op === "state" && !this.ready) { this.close(true); return; }
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
      if (child.op === "call" || child.op === "slot") {
        if (!this.ready) { this.close(true); return; }
        if (child.op === "call") this.call(child); else this.slot(child);
      }
      // Opaque local draft state is deliberately not persisted in the first release.
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
export class ScreenChannel extends FrameChannel {
  /** Set once, by the child's `ready`: the PUSH shape it negotiated. */
  private shape: Shape = "v1";
  /** The image refs of the last PUSH actually sent — the only refs the
   *  child may request. */
  private refs = new Set<string>();
  /** Outstanding refs (queued or loading), deduplicated. */
  private assets = new Set<string>();
  private queue: string[] = [];
  private loading = 0;
  constructor(source: Window, receipt: MountReceipt, private projection: ScreenPush,
    removeFrame: () => void, failed: () => void, onReady: () => void,
    private loadAsset: AssetLoader = async () => null, private actions?: ScreenActions,
    private clock?: () => number) {
    super(source, receipt, removeFrame, failed, onReady);
  }
  private calls = 0;
  private slots: SlotController | null = null;
  /** Only a v2 child with an action host; the verb set was closed by the parser. */
  protected call(request: CallRequest): void {
    const actions = this.actions;
    if (this.shape !== "v2" || !actions || this.calls >= CALL_QUEUE_MAX) { this.close(true); return; }
    this.calls++;
    const reply = (message: ReplyMessage) => { if (!this.closed) this.port?.postMessage(message); };
    void actions.call(request.verb, request.args, { showLink: url => actions.onLink(url) })
      .catch((): ActionResult => ({ ok: false, refusal: { code: "failed", text: "That didn't work." } }))
      .then(result => {
        this.calls--;
        if (!result.ok) { reply({ v: 2, op: "reply", id: request.id, ok: false, refusal: result.refusal }); return; }
        const size = new TextEncoder().encode(JSON.stringify(result.data ?? null)).byteLength;
        if (size > REPLY_BYTES_MAX) reply({ v: 2, op: "reply", id: request.id, ok: false, refusal: { code: "too_large", text: "That is too large to show." } });
        else reply({ v: 2, op: "reply", id: request.id, ok: true, data: result.data ?? null });
      });
  }
  /** Only a v2 child with an action host. At most one live slot per frame. */
  protected slot(request: SlotRequest): void {
    const actions = this.actions;
    if (this.shape !== "v2" || !actions) { this.close(true); return; }
    this.slots ??= new SlotController(message => { if (!this.closed) this.port?.postMessage(message); },
      view => actions.onSlot(view), actions.planner, this.clock);
    void this.slots.request(request);
  }
  /** A press on the host-drawn button (the board component calls this). */
  tapSlot(token: string, trusted: boolean, pressedAt: number): boolean {
    return !this.closed && (this.slots?.tap(token, trusted, pressedAt) ?? false);
  }
  close(report = false): void {
    this.slots?.close();
    super.close(report);
  }
  protected accept(ready: Extract<ChildToHost, { op: "ready" }>): boolean {
    // Only the opt-ins a workspace screen can honour; the chat opt-in closes it.
    const shape = shapeOf(ready.accepts);
    if (!shape) return false;
    this.shape = shape;
    return true;
  }
  /** Only a v2 child, for a ref the last PUSH carried; anything else closes. */
  protected asset(ref: string): void {
    if (this.shape !== "v2" || !this.refs.has(ref) ||
        (!this.assets.has(ref) && this.assets.size >= ASSET_QUEUE_MAX)) { this.close(true); return; }
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
  /** Send the negotiated shape, or close when it cannot be sent within bounds. */
  protected send(): void {
    const push = shapeFor(this.projection, this.shape);
    if (!push) { this.close(true); return; }
    this.refs = this.shape === "v2" ? pushedAssetRefs(push) : new Set();
    this.port?.postMessage(push);
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
