import { legacyPush, parseChild, parseInit, type ScreenPush } from "./screenProtocol";

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

/** Each instance owns one frame generation. Teardown precedes replacement. */
export class ScreenChannel {
  private port: MessagePort | null = null;
  private ready = false;
  /** Set once, by the child's `ready`: whether it opted into publish-intents.v1. */
  private intents = false;
  private closed = false;
  private initialized = false;
  private loads = 0;
  constructor(private source: Window, private receipt: MountReceipt,
    private projection: ScreenPush, private removeFrame: () => void,
    private failed: () => void, private onReady: () => void) {}
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
        if (this.ready) { this.close(true); return; }
        this.ready = true;
        this.intents = child.accepts !== undefined;
        this.port?.postMessage(this.shaped());
        this.onReady();
      }
      // Opaque local draft state is deliberately not persisted in the first release.
    };
    this.port.onmessageerror = () => this.close(true);
    this.port.start();
  }
  update(projection: ScreenPush): void {
    if (this.closed) return;
    if (projection.install_id !== this.projection.install_id || projection.digest !== this.projection.digest ||
        projection.tag !== this.projection.tag || projection.context_id !== this.projection.context_id) {
      this.close(true); return;
    }
    this.projection = projection;
    if (this.ready) this.port?.postMessage(this.shaped());
  }
  private shaped(): ScreenPush { return this.intents ? this.projection : legacyPush(this.projection); }
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
