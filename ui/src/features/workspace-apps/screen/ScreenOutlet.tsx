import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { sessionHeaders } from "../../../lib/sessionKey";
import { parseMount, ScreenChannel, type AssetLoader, type ScreenActions } from "./screenLifecycle";
import { invokeTool, revokeTool } from "./screenActions";
import { type SlotView } from "./screenSlot";
import ScreenSlotLayer from "./ScreenSlotLayer";
import { parseDraftImageRef, type ScreenPush } from "./screenProtocol";
import { workspaceApps } from "../workspaceApps";
import { downscaleToDataUrl } from "./screenAssets";

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
    // The live mount's action token, held host-side for invoke relay and
    // revoked on teardown so a stale mount's handle cannot be reused.
    let actionToken: string | null = null;
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
          body: JSON.stringify(current.context_id ? { context_id: current.context_id } : {}),
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
        // CAD-1177: hold the mount's action token host-side. It is never
        // rendered into the frame and never crosses the MessagePort — a
        // `tool` op only carries alias/input/request_id, and the host
        // appends this token on the invoke relay.
        actionToken = receipt.action_token ?? null;
        owned = new ScreenChannel(frame.contentWindow, receipt, latest.current,
          () => frame.remove(), fail, () => { if (!retired) { clearTimeout(timer); setState("ready"); } },
          async ref => {
            const draft=parseDraftImageRef(ref);
            if(draft&&actionToken){
              const image=await workspaceApps.socialDraftAsset({action_token:actionToken,alias:draft.alias,draft_id:draft.draftId});
              const bytes=new Uint8Array(image.bytes);let binary="";
              for(let offset=0;offset<bytes.length;offset+=0x8000) binary+=String.fromCharCode(...bytes.subarray(offset,offset+0x8000));
              return downscaleToDataUrl(btoa(binary),image.mime);
            }
            return loader.current ? loader.current(ref) : null;
          },
          acting.current ? {
            call: (verb, args, ui) => acting.current!.call(verb, args, ui),
            planner: (verb, args) => acting.current!.planner(verb, args, actionToken ? { actionToken } : undefined),
            onSlot: view => { if (!retired) setSlot(view); },
            onLink: url => { if (!retired) setLink(url); },
            ...(actionToken ? { invoke: (alias: string, input: Record<string, unknown>, requestId: string, scope?:import("./screenProtocol").GenerationScope) =>
              invokeTool(actionToken!, alias, input, requestId, scope) } : {}),
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
      // Retire the mount's action handle server-side so a torn-down or
      // remounted screen's token cannot be replayed. Best-effort — the
      // daemon's own consume/supersede already refuses a stale mount.
      if (actionToken) {
        const token = actionToken;
        actionToken = null;
        void revokeTool(token);
      }
    };
  }, [scope]);
  useEffect(() => { channel.current?.update(projection); }, [projection]);
  return <>
    <div aria-label="Installed app screen" style={{ position: "relative", flex: "1 1 0%", minHeight: 0, height: "100%", display: state === "ready" ? "block" : "none" }}>
      <div ref={container} style={{ position: "absolute", inset: 0 }} />
      <ScreenSlotLayer view={slot} link={link} onDismissLink={() => setLink(null)}
        onTap={(token, trusted, at) => { channel.current?.tapSlot(token, trusted, at); }}
        onCancel={token => channel.current?.cancelSlot(token)} />
    </div>
    {/* CAD-1137: a quiet first-mount skeleton, with the blank frame hidden until handshake readiness. */}
    {state === "loading" && (
      <div className="wa-skeleton" role="status" aria-label="Loading installed app" style={{ padding: "0.5rem 0.25rem" }}>
        <span className="sr-only">Loading installed app…</span>
        <div className="wa-skel-tabs">
          {[3, 3.5, 4.5, 3.5].map((w, i) => (
            <span key={i} className="wa-skel wa-skel-pulse" style={{ width: `${w}rem`, height: "0.85rem" }} />
          ))}
        </div>
        <div className="wa-skel-board">
          {[0, 1, 2, 3].map((i) => (
            <div key={i} className="wa-skel-lane">
              <span className="wa-skel wa-skel-pulse" style={{ width: "45%", height: "0.8rem" }} />
              <span className="wa-skel wa-skel-pulse" style={{ width: "100%", height: "4.5rem" }} />
              <span className="wa-skel wa-skel-pulse" style={{ width: "100%", height: "4.5rem" }} />
            </div>
          ))}
        </div>
      </div>
    )}
    {state === "fallback" && fallback}
  </>;
}
