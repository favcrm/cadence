import { parseChild, parseInit, PUBLISH_INTENTS_V1, shapeFor, type ChildToHost, type ScreenPush } from "./screenProtocol";

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
  /** Set once, by the child's `ready`: whether it opted into publish-intents.v1. */
  private intents = false;
  constructor(source: Window, receipt: MountReceipt, private projection: ScreenPush,
    removeFrame: () => void, failed: () => void, onReady: () => void) {
    super(source, receipt, removeFrame, failed, onReady);
  }
  protected accept(ready: Extract<ChildToHost, { op: "ready" }>): boolean {
    // Only the opt-ins a workspace screen can honour; the chat opt-in closes it.
    if (ready.accepts !== undefined && ready.accepts[0] !== PUBLISH_INTENTS_V1) return false;
    this.intents = ready.accepts !== undefined;
    return true;
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
    const push = shapeFor(this.projection, this.intents);
    if (!push) { this.close(true); return; }
    this.port?.postMessage(push);
  }
}
