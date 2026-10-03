import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { sessionHeaders } from "../../../lib/sessionKey";
import { parseMount } from "../../workspace-apps/screen/screenLifecycle";
import { ChatChannel, type FrameDirective } from "./chatChannel";
import type { FrameSize } from "./contract";

const HEIGHT: Record<FrameSize, number> = { small: 160, medium: 320, large: 480 };
const READY_MS = 10_000;

/**
 * A directive's own sandboxed screen, inline in chat (CAD-1110, Tier 2). The
 * host is the only creator of the frame: `sandbox="allow-scripts"` (no
 * same-origin, navigation, forms, popups or modals), `no-referrer`, a document
 * fetched with the one-use FrameCap the operator-session mount POST returned
 * for THIS pane's installation and the tag the descriptor names. The cap and
 * the session never enter the frame or any message. Any failure (refused
 * mount, bad receipt, no `ready` within 10 s, a closed port) reports once and
 * the row falls back to the message's plain text.
 */
export default function ChatFrame({
  installId,
  tag,
  size,
  directive,
  onFail,
}: {
  installId: string;
  tag: string;
  size: FrameSize;
  directive: FrameDirective;
  onFail: (tag: string) => void;
}) {
  const container = useRef<HTMLDivElement>(null);
  const channel = useRef<ChatChannel | null>(null);
  const latest = useRef(directive);
  latest.current = directive;
  const failRef = useRef(onFail);
  failRef.current = onFail;
  const [state, setState] = useState<"loading" | "ready">("loading");
  useLayoutEffect(() => {
    const controller = new AbortController();
    let retired = false;
    let owned: ChatChannel | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    setState("loading");
    const fail = () => {
      if (!retired) failRef.current(tag);
    };
    const onMessage = (event: MessageEvent) => owned?.receive(event);
    window.addEventListener("message", onMessage);
    void (async () => {
      try {
        const response = await fetch(
          `/api/app-installations/${encodeURIComponent(installId)}/screens/${encodeURIComponent(tag)}/mount`,
          {
            method: "POST", credentials: "same-origin", cache: "no-store", signal: controller.signal,
            headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
            body: "{}",
          },
        );
        if (!response.ok) throw new Error("Preview mount refused");
        const receipt = parseMount(await response.json(), tag);
        if (!receipt) throw new Error("Invalid preview mount receipt");
        if (retired || !container.current) return;
        const frame = document.createElement("iframe");
        frame.title = "App preview";
        frame.setAttribute("sandbox", "allow-scripts");
        frame.referrerPolicy = "no-referrer";
        frame.style.cssText = "display:block;width:100%;height:100%;border:0;min-height:0";
        frame.src = receipt.mount;
        container.current.replaceChildren(frame);
        if (!frame.contentWindow) throw new Error("Preview frame unavailable");
        owned = new ChatChannel(frame.contentWindow, receipt, latest.current,
          () => frame.remove(), fail, () => { if (!retired) { clearTimeout(timer); setState("ready"); } });
        channel.current = owned;
        frame.addEventListener("load", () => owned?.load());
        timer = setTimeout(() => { owned?.close(true); }, READY_MS);
      } catch {
        if (!retired) { owned?.close(); container.current?.replaceChildren(); fail(); }
      }
    })();
    return () => {
      retired = true;
      controller.abort();
      clearTimeout(timer);
      window.removeEventListener("message", onMessage);
      owned?.close();
      channel.current = null;
      container.current?.replaceChildren();
    };
  }, [installId, tag]);
  useEffect(() => { channel.current?.update(directive); }, [directive]);
  return (
    <div data-chat-frame={tag} data-state={state} className="app-chat-frame">
      {state === "loading" && <p className="text-micro text-ink-500" role="status">Loading preview…</p>}
      <div ref={container} style={{ height: HEIGHT[size], display: state === "ready" ? "block" : "none" }} />
    </div>
  );
}
