import { useEffect, useLayoutEffect, useRef, useState, type ReactNode } from "react";
import { sessionHeaders } from "../../../lib/sessionKey";
import { beginPrivateOperation, onPrivateReadsPaused, privateFetch, privateReadsAllowed, privateSessionGeneration, type PrivateOperation } from "../../../lib/privateSession";
import { compareShellVersions, parseMount, ScreenChannel, shellVersion, type AssetLoader, type MediaChatAttach, type ScreenActions } from "./screenLifecycle";
import type { ScreenFailure, ScreenPaneState } from "./screenFailure";
import { invokeTool, revokeTool } from "./screenActions";
import { type SlotView } from "./screenSlot";
import ScreenSlotLayer from "./ScreenSlotLayer";
import { parseDraftImageRef, type ScreenPush } from "./screenProtocol";
import { workspaceApps } from "../workspaceApps";
import { useLocale } from "../../../lib/locale";
import { appliedFrameTheme, onAppliedThemeChange, type FrameTheme } from "../../../lib/theme";
import { downscaleToDataUrl } from "./screenAssets";
import { registerNavigationGuard, useHref } from "../../../lib/useLocation";
import { recordServices, object, exact, type HostCommand } from "./screenServices";
import { admitDisplaySession, purgeDisplayCache, withReadObservations } from "./screenDisplayCache";
import InertPreviewLayer, { type InertPreview } from "./InertPreviewLayer";
import { parseRoutePatch, routeFromHref, type RoutePatch } from "./screenRoute";
import { RECORD_CALL_VERBS, RECORD_SLOT_VERBS, type RecordCallVerb, type RecordSlotVerb, type ScreenNavigation, type ScreenPresentationMode } from "./screenProtocol";
import type { MountReceipt } from "./screenLifecycle";

/** The pane renderer: the outlet's typed failure and a retry that re-mounts
 *  this same projection without a reload. */
export type ScreenFailurePane = (failure: ScreenPaneState, retry: () => void) => ReactNode;

/** The tenant's structured refusal carries `error.code`; anything else is null. */
function mountRefusalCode(body: unknown): string | null {
  if (!body || typeof body !== "object" || Array.isArray(body) || !("error" in body)) return null;
  const error = body.error;
  if (!error || typeof error !== "object" || Array.isArray(error) || !("code" in error)) return null;
  return typeof error.code === "string" ? error.code : null;
}

/** App code lives in its independently installed bundle, never in the board. */
export default function ScreenOutlet({ projection, fallback, renderFailure, loadAsset, actions, onRoute, onCommand, onNavigation, onMediaAttach, onScreenDrawer, presentation }: {
  projection: ScreenPush; fallback: ReactNode;
  /** Render the specific pane for a mount failure; absent keeps `fallback`. */
  renderFailure?: ScreenFailurePane;
  loadAsset?: AssetLoader;
  /** The operator's verbs for this frame (HP3); absent for a non-operator, so every `call`/`slot` closes it. */
  actions?: Pick<ScreenActions, "call" | "planner">;
  onRoute?: (patch: RoutePatch, navigation: ScreenNavigation) => void;
  onCommand?: (command: HostCommand) => void;
  onNavigation?: (navigation: ScreenNavigation | null) => void;
  onMediaAttach?: MediaChatAttach;
  /** CAD-1615 — the shell opens and closes the board-wide drawer. */
  onScreenDrawer?: (mode: ScreenPresentationMode, surface?: string) => void;
  /** CAD-1615 — which surface this frame is presented in. */
  presentation?: ScreenPresentationMode;
}) {
  const { t, locale } = useLocale();
  const presentationLocale = useRef(locale);
  presentationLocale.current = locale;
  const href = useHref();
  const navigationCallbacks = useRef({ onRoute, onCommand, onNavigation, onMediaAttach, onScreenDrawer, href });
  navigationCallbacks.current = { onRoute, onCommand, onNavigation, onMediaAttach, onScreenDrawer, href };
  const mounted = useRef<MountReceipt | null>(null);
  const decorate = (p: ScreenPush, receipt: MountReceipt): ScreenPush => receipt.host_contract === "screen-records.v1" ? withReadObservations({ ...p, presentation_locale: presentationLocale.current, route: routeFromHref(receipt.navigation, receipt.tag, navigationCallbacks.current.href), actions: receipt.operations ?? [] }, receipt.display_scope_key!, receipt.operations ?? []) : p;
  const container = useRef<HTMLDivElement>(null);
  const channel = useRef<ScreenChannel | null>(null);
  const latest = useRef(projection);
  latest.current = projection;
  const loader = useRef(loadAsset);
  loader.current = loadAsset;
  const acting = useRef(actions);
  acting.current = actions;
  const [preview, setPreview] = useState<InertPreview | null>(null);
  const [remoteImages, setRemoteImages] = useState<string[]>([]);
  const [slot, setSlot] = useState<SlotView | null>(null);
  const [link, setLink] = useState<string | null>(null);
  const [state, setState] = useState<"loading" | "ready" | "fallback">("loading");
  // The board's applied pick, carried into the mount: a sandboxed frame is its
  // own document and never sees the board's `data-theme`. A change re-mints the
  // receipt (the effect below depends on it), so the frame is never left on a
  // stale theme.
  const [frameTheme, setFrameTheme] = useState<FrameTheme | null>(() => appliedFrameTheme());
  useEffect(() => onAppliedThemeChange(setFrameTheme), []);
  const [failure, setFailure] = useState<ScreenPaneState | null>(null);
  const [attempt, setAttempt] = useState(0);
  /** Retry re-mounts the same projection: the effect's scope identity is
   *  unchanged, so only the attempt counter rebuilds the frame. */
  const retry = () => { setFailure(null); setState("loading"); setAttempt(value => value + 1); };
  // CAD-1615 — the surface the shell presents this frame in reaches the
  // child over the same port. The host owns the geometry; the child draws
  // its surface there.
  useEffect(() => {
    if (!presentation) return;
    channel.current?.present(presentation);
  }, [presentation, state]);
  const scope = JSON.stringify([projection.install_id, projection.digest, projection.tag, projection.context_id]);
  useLayoutEffect(() => {
    const generation = privateSessionGeneration();
    const controller = new AbortController();
    const unsubscribe = onPrivateReadsPaused(() => controller.abort());
    let operation: PrivateOperation | null = null;
    try {
      operation = beginPrivateOperation(controller.signal);
    } catch {
      unsubscribe();
      return;
    }
    if (operation === null) return;
    const session = operation;
    let retired = false;
    let owned: ScreenChannel | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // The live mount's action token, held host-side for invoke relay and
    // revoked on teardown so a stale mount's handle cannot be reused.
    let actionToken: string | null = null;
    let services: ReturnType<typeof recordServices> | null = null;
    const isCurrent = () => !retired && !controller.signal.aborted && !session.signal.aborted && privateReadsAllowed() && generation === privateSessionGeneration() && JSON.stringify([latest.current.install_id, latest.current.digest, latest.current.tag, latest.current.context_id]) === scope;
    let releaseLeaveGuard: (() => void) | null = null;
    const warnBeforeUnload = (event: BeforeUnloadEvent) => {
      if (!releaseLeaveGuard || !isCurrent() || owned !== channel.current) return;
      event.preventDefault();
      event.returnValue = "";
    };
    const clearLeaveGuard = () => {
      releaseLeaveGuard?.();
      releaseLeaveGuard = null;
      window.removeEventListener("beforeunload", warnBeforeUnload);
    };
    const command = (c: HostCommand) => {
      if (!isCurrent() || !mounted.current) return;
      if (c.operation === "screen.route.patch") {
        const nav = mounted.current.navigation;
        const patch = nav ? parseRoutePatch(c.args, nav) : null;
        if (!nav || !patch || !navigationCallbacks.current.onRoute) throw new Error("Unsupported screen route");
        navigationCallbacks.current.onRoute(patch, nav);
      } else if (c.operation === "screen.preview.show") {
        const a = c.args;
        const anchor = a.anchor;
        if (!exact(a, ["preview_id", "document", "anchor", "mode"]) || typeof a.preview_id !== "string" || !/^[A-Za-z0-9_-]{1,128}$/.test(a.preview_id) || typeof a.document !== "string" || new TextEncoder().encode(a.document).byteLength > 49152 || a.mode !== "draft" && a.mode !== "saved" || anchor !== "footer" && (!object(anchor) || Object.keys(anchor).sort().join() !== "h,w,x,y" || ![anchor.x, anchor.y].every(n => typeof n === "number" && Number.isFinite(n) && Math.abs(n) <= 16384) || ![anchor.w, anchor.h].every(n => typeof n === "number" && Number.isFinite(n) && n >= 0 && n <= 16384))) throw new Error("Invalid preview command");
        setPreview({ id: a.preview_id, document: a.document, anchor: anchor as InertPreview["anchor"], mode: a.mode });
      } else if (c.operation === "screen.preview.clear") {
        if (!exact(c.args, ["preview_id"]) || typeof c.args.preview_id !== "string") throw new Error("Invalid preview clear");
        setPreview(p => p?.id === c.args.preview_id ? null : p);
      } else if (["smtp.manage", "chat.campaign.open", "chat.campaign.brief", "chat.whatsapp.open", "chat.whatsapp.brief"].includes(c.operation)) {
        if (!navigationCallbacks.current.onCommand) throw new Error("Host command unavailable");
        navigationCallbacks.current.onCommand(c);
      } else throw new Error("Unsupported host command");
    };
    setState("loading");
    setFailure(null);
    setSlot(null); setLink(null); setPreview(null); setRemoteImages([]);
    const fail = (why: ScreenPaneState) => { clearLeaveGuard(); if (isCurrent()) { setFailure(why); services?.retire(); mounted.current = null; navigationCallbacks.current.onNavigation?.(null); setPreview(null); setSlot(null); setState("fallback"); } };
    const onMessage = (event: MessageEvent) => owned?.receive(event);
    window.addEventListener("message", onMessage);
    void (async () => {
      let pending: ScreenFailure = { kind: "frame_unavailable" };
      try {
        const current = latest.current;
        const response = await privateFetch(`/api/app-installations/${encodeURIComponent(current.install_id)}/screens/${encodeURIComponent(current.tag)}/mount`, {
          method: "POST", credentials: "same-origin", cache: "no-store", signal: controller.signal,
          headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() },
          // The frame is its own document and cannot see the board's
          // `data-theme`; the mount carries the applied pick, and "system"
          // omits it so the frame's own media query decides.
          body: JSON.stringify({
            ...(current.context_id ? { context_id: current.context_id } : {}),
            ...(frameTheme ? { theme: frameTheme } : {}),
          }),
        }, session);
        if (!response.ok) {
          if ([401, 403].includes(response.status)) purgeDisplayCache();
          pending = { kind: "refused", status: response.status, code: mountRefusalCode(await response.json().catch(() => null)) };
          throw new Error("Screen mount refused");
        }
        const receipt = parseMount(await response.json(), current.tag);
        if (!receipt) { pending = { kind: "invalid_receipt" }; throw new Error("Invalid screen mount receipt"); }
        session.assertCurrent();
        if (receipt.host_contract === "screen-records.v1" && (receipt.install_id !== current.install_id || receipt.context_id !== current.context_id || receipt.digest !== current.digest)) { pending = { kind: "invalid_receipt" }; throw new Error("Screen receipt scope changed"); }
        if (receipt.min_shell && compareShellVersions(receipt.min_shell, shellVersion) > 0) {
          fail({ kind: "needs_host_update", required: receipt.min_shell, current: shellVersion });
          return;
        }
        if (!isCurrent() || !container.current) return;
        mounted.current = receipt;
        if (receipt.host_contract === "screen-records.v1") {
          admitDisplaySession(receipt.display_session_key!);
          setRemoteImages(receipt.remote_images ?? []);
          services = recordServices(receipt, JSON.stringify([current.install_id, current.context_id]), { current: isCurrent, command, refused: () => { owned?.close(true); } });
        }
        const frame = document.createElement("iframe");
        frame.title = current.installation.title;
        frame.setAttribute("sandbox", "allow-scripts");
        frame.referrerPolicy = "no-referrer";
        frame.style.cssText = "display:block;width:100%;height:100%;border:0;min-height:0";
        // The frame URL is what survives a restart or a reload, so the pick
        // rides there too: a cap minted before the theme existed still gets
        // the right document. `system` stays query-less (the OS decides).
        frame.src = frameTheme ? `${receipt.mount}?theme=${frameTheme}` : receipt.mount;
        // The container has no earlier frame: previous layout cleanup removed it.
        container.current.replaceChildren(frame);
        if (!frame.contentWindow) { pending = { kind: "frame_unavailable" }; throw new Error("Screen frame unavailable"); }
        // CAD-1177: hold the mount's action token host-side. It is never
        // rendered into the frame and never crosses the MessagePort — a
        // `tool` op only carries alias/input/request_id, and the host
        // appends this token on the invoke relay.
        actionToken = receipt.action_token ?? null;
        owned = new ScreenChannel(frame.contentWindow, receipt, decorate(latest.current, receipt),
          () => { clearLeaveGuard(); frame.remove(); }, () => fail({ kind: "frame_unavailable" }), () => { if (isCurrent()) { clearTimeout(timer); setState("ready"); navigationCallbacks.current.onNavigation?.(receipt.navigation ?? null); } },
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
          services || acting.current ? {
            call: (verb, args, ui) => {
              if (services) return (RECORD_CALL_VERBS as readonly string[]).includes(verb)
                ? services.call(verb as RecordCallVerb, args)
                : Promise.resolve({ ok: false as const, refusal: { code: "forbidden", text: "This operation is unavailable." } });
              const attach = navigationCallbacks.current.onMediaAttach;
              return acting.current!.call(verb, args, {
                ...ui,
                attachMedia: attach ? (selected, token) => attach(selected, token, isCurrent) : undefined,
              });
            },
            planner: (verb, args) => services ? (RECORD_SLOT_VERBS as readonly string[]).includes(verb) ? services.planner(verb as RecordSlotVerb, args) : Promise.resolve({ code: "forbidden", text: "This operation is unavailable." }) : acting.current!.planner(verb, args, actionToken ? { actionToken } : undefined),
            onSlot: view => { if (isCurrent()) setSlot(view); },
            onLink: url => { if (isCurrent()) setLink(url); },
            onScreenDrawer: onScreenDrawer
              ? (mode, surface) => { if (isCurrent()) navigationCallbacks.current.onScreenDrawer?.(mode, surface); }
              : undefined,
            ...(actionToken ? { invoke: (alias: string, input: Record<string, unknown>, requestId: string, scope?:import("./screenProtocol").GenerationScope) =>
              invokeTool(actionToken!, alias, input, requestId, scope) } : {}),
          } : undefined, undefined, unsaved => {
            if (!isCurrent() || mounted.current !== receipt || channel.current !== owned) return;
            clearLeaveGuard();
            if (!unsaved) return;
            releaseLeaveGuard = registerNavigationGuard(() => !isCurrent() || channel.current !== owned ||
              window.confirm("Your email draft has not been saved. Leave this page and discard those local edits?"));
            window.addEventListener("beforeunload", warnBeforeUnload);
          });
        channel.current = owned;
        frame.addEventListener("load", () => owned?.load());
        timer = setTimeout(() => {
          if (!isCurrent() || owned === null || owned !== channel.current) return;
          fail({ kind: "handshake_timeout" });
          owned.close();
        }, 10000);
      } catch {
        if (!retired && !session.signal.aborted && !controller.signal.aborted && privateReadsAllowed() && generation === privateSessionGeneration()) {
          owned?.close(); container.current?.replaceChildren(); fail(pending);
        }
      }
    })();
    return () => {
      retired = true;
      unsubscribe();
      session.dispose();
      clearLeaveGuard();
      services?.retire();
      mounted.current = null;
      navigationCallbacks.current.onNavigation?.(null);
      controller.abort();
      clearTimeout(timer);
      window.removeEventListener("message", onMessage);
      owned?.close();
      channel.current = null;
      setSlot(null); setLink(null); setPreview(null);
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
  }, [scope, attempt, frameTheme]);
  useEffect(() => { if (mounted.current) channel.current?.update(decorate(projection, mounted.current)); }, [projection, href, locale]);
  return <>
    <div aria-label={t("Installed app screen")} style={{ position: "relative", flex: "1 1 0%", minHeight: 0, height: "100%", display: state === "ready" ? "block" : "none" }}>
      <div ref={container} style={{ position: "absolute", inset: 0 }} />
      <InertPreviewLayer preview={preview} remoteImages={remoteImages} />
      <ScreenSlotLayer view={slot} link={link} onDismissLink={() => setLink(null)}
        onTap={(token, trusted, at, input) => { channel.current?.tapSlot(token, trusted, at, input); }}
        onCancel={token => channel.current?.cancelSlot(token)} />
    </div>
    {/* CAD-1137: a quiet first-mount skeleton, with the blank frame hidden until handshake readiness. */}
    {state === "loading" && (
      <div className="wa-skeleton wa-screen-skeleton" role="status" aria-label={t("Loading installed app")} style={{ padding: "0.5rem 0.25rem" }}>
        <span className="sr-only">{t("Loading installed app…")}</span>
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
    {state === "fallback" && (failure && renderFailure ? renderFailure(failure, retry) : fallback)}
  </>;
}
