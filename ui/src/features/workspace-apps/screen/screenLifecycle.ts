import { ASSET_BYTES_MAX, ASSET_DATA_URL, parseChild, parseInit, pushedAssetRefs, shapeFor, shapeOf,
  type AssetReply, type ChildToHost, type ScreenPush, type Shape } from "./screenProtocol";

/** Resolves one pushed asset ref to a downscaled `data:` URL, or `null`
 *  when it cannot be loaded now. Supplied by the board; never by the frame. */
export type AssetLoader = (ref: string) => Promise<string | null>;
/** Most asset requests one frame may have outstanding (the package's
 *  per-mount budget); one more closes it. At most ASSET_PARALLEL load at once. */
export const ASSET_QUEUE_MAX = 256;
const ASSET_PARALLEL = 4;

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
    private loadAsset: AssetLoader = async () => null) {
    super(source, receipt, removeFrame, failed, onReady);
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
