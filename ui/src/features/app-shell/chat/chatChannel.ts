import { FrameChannel, type MountReceipt } from "../../workspace-apps/screen/screenLifecycle";
import {
  CHAT_DIRECTIVE_V1,
  CHAT_PUSH_BYTES_MAX,
  type ChatDirectivePush,
  type ChildToHost,
} from "../../workspace-apps/screen/screenProtocol";
import type { Payload } from "./directive";

/**
 * One chat frame's channel (CAD-1110, Tier 2). Everything that makes a
 * frame safe — opaque origin, single init, tag/nonce/generation, the closed
 * `ready`/`state` child vocabulary — is the CAD-1006 receive side it
 * extends, unchanged. Chat adds exactly two things: the child must opt in
 * with `accepts:["chat-directive.v1"]` (a bare or publish-intents `ready`
 * closes it), and the ONLY host-to-frame message is the directive push: the
 * matched kind plus the flat payload the matcher validated, at most 4 KiB.
 * No scope projection, no id, no URL ever reaches a chat frame.
 */
export interface FrameDirective {
  kind: string;
  data: Payload;
}

export class ChatChannel extends FrameChannel {
  constructor(source: Window, receipt: MountReceipt, private directive: FrameDirective,
    removeFrame: () => void, failed: () => void, onReady: () => void) {
    super(source, receipt, removeFrame, failed, onReady);
  }
  protected accept(ready: Extract<ChildToHost, { op: "ready" }>): boolean {
    return ready.accepts?.[0] === CHAT_DIRECTIVE_V1;
  }
  /** A newer directive for the same mount: push-only, no remount. */
  update(directive: FrameDirective): void {
    if (this.closed) return;
    this.directive = directive;
    if (this.ready) this.send();
  }
  protected send(): void {
    const push: ChatDirectivePush = {
      v: 1,
      op: "directive",
      tag: this.receipt.tag,
      kind: this.directive.kind,
      data: this.directive.data,
    };
    if (new TextEncoder().encode(JSON.stringify(push)).byteLength > CHAT_PUSH_BYTES_MAX) {
      this.close(true);
      return;
    }
    this.port?.postMessage(push);
  }
}
