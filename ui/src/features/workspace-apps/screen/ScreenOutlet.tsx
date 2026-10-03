import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { sessionHeaders } from "../../../lib/sessionKey";
import { parseMount, ScreenChannel, type AssetLoader, type ScreenActions } from "./screenLifecycle";
import { type SlotView } from "./screenSlot";
import ScreenSlotLayer from "./ScreenSlotLayer";
import type { ScreenPush } from "./screenProtocol";

/** App code lives in its independently installed bundle, never in the board. */
export default function ScreenOutlet({ projection, fallback, loadAsset, actions }: {
  projection: ScreenPush; fallback: ReactNode; loadAsset?: AssetLoader;
  /** The operator's verbs for this frame (HP3); absent for a non-operator, so every `call`/`slot` closes it. */
  actions?: Pick<ScreenActions, "call" | "planner">;
}) {
  const container = useRef<HTMLDivElement>(null);
  const channel = useRef<ScreenChannel | null>(null);
  const latest = useRef(projection);
  latest.current = projection;
  const loader = useRef(loadAsset);
  loader.current = loadAsset;
  const acting = useRef(actions);
  acting.current = actions;
  const [slot, setSlot] = useState<SlotView | null>(null);
  const [link, setLink] = useState<string | null>(null);
  const [state, setState] = useState<"loading" | "ready" | "fallback">("loading");
  const scope = JSON.stringify([projection.install_id, projection.digest, projection.tag, projection.context_id]);
  useLayoutEffect(() => {
    const controller = new AbortController();
    let retired = false;
    let owned: ScreenChannel | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    setState("loading");
    setSlot(null); setLink(null);
    const fail = () => { if (!retired) setState("fallback"); };
    const onMessage = (event: MessageEvent) => owned?.receive(event);
    window.addEventListener("message", onMessage);
    void (async () => {
      try {
        const current = latest.current;
        const response = await fetch(`/api/app-installations/${encodeURIComponent(current.install_id)}/screens/${encodeURIComponent(current.tag)}/mount`, {
          method: "POST", credentials: "same-origin", cache: "no-store", signal: controller.signal,
          headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
          body: "{}",
        });
        if (!response.ok) throw new Error("Screen mount refused");
        const receipt = parseMount(await response.json(), current.tag);
        if (!receipt) throw new Error("Invalid screen mount receipt");
        if (retired || !container.current) return;
        const frame = document.createElement("iframe");
        frame.title = current.installation.title;
        frame.setAttribute("sandbox", "allow-scripts");
        frame.referrerPolicy = "no-referrer";
        frame.style.cssText = "display:block;width:100%;height:100%;border:0;min-height:0";
        frame.src = receipt.mount;
        // The container has no earlier frame: previous layout cleanup removed it.
        container.current.replaceChildren(frame);
        if (!frame.contentWindow) throw new Error("Screen frame unavailable");
        owned = new ScreenChannel(frame.contentWindow, receipt, latest.current,
          () => frame.remove(), fail, () => { if (!retired) { clearTimeout(timer); setState("ready"); } },
          ref => loader.current ? loader.current(ref) : Promise.resolve(null),
          acting.current ? {
            call: (verb, args, ui) => acting.current!.call(verb, args, ui),
            planner: (verb, args) => acting.current!.planner(verb, args),
            onSlot: view => { if (!retired) setSlot(view); },
            onLink: url => { if (!retired) setLink(url); },
          } : undefined);
        channel.current = owned;
        frame.addEventListener("load", () => owned?.load());
        timer = setTimeout(() => { owned?.close(true); }, 10000);
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
      setSlot(null); setLink(null);
      container.current?.replaceChildren();
    };
  }, [scope]);
  useEffect(() => { channel.current?.update(projection); }, [projection]);
  return <>
    <div aria-label="Installed app screen" style={{ position: "relative", flex: "1 1 0%", minHeight: 0, height: "100%", display: state === "fallback" ? "none" : "block" }}>
      <div ref={container} style={{ position: "absolute", inset: 0 }} />
      <ScreenSlotLayer view={slot} link={link} onDismissLink={() => setLink(null)}
        onTap={(token, trusted, at) => { channel.current?.tapSlot(token, trusted, at); }} />
    </div>
    {state === "loading" && <p role="status">Loading installed app…</p>}
    {state === "fallback" && fallback}
  </>;
}
