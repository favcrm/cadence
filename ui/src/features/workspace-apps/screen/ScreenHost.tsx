import { useCallback, useEffect, useMemo, useRef, useState, type ComponentProps, type ReactNode } from "react";
import { readWorkspaceAppSnapshot, readWorkspaceAppSnapshotFresh, type Installation, type AppContext, type WorkspaceAppSnapshot } from "../workspaceApps";
import { runCall, makePlanner } from "./screenActions";
import { useHref } from "../../../lib/useLocation";
import ScreenOutlet from "./ScreenOutlet";
import { screenProjection } from "./screenProjection";
import { useScreenExtras } from "./useScreenExtras";
import type { AssetLoader, ScreenActions } from "./screenLifecycle";
import { ACTION_VERBS, type ScreenPush } from "./screenProtocol";

/**
 * Host around a mounted app screen. The app screen is all a viewer sees: no
 * host tab bar (CAD-1040 removed the CAD-1026 toggle). `?screen=native` is
 * the operator's back door to the native workspace (WorkspaceApp renders
 * nothing here for a non-operator). Native unmounts the outlet, so its frame
 * and port close exactly as on a context switch, and dropping the parameter
 * mounts a fresh one.
 *
 * CAD-1137: the outlet's own remount scope is the frame's identity —
 * (install, bundle digest, screen tag, context) — so the workspace's
 * in-app tab switches (Home, Runs, Needs you, Settings…) re-render the
 * host but keep the mounted frame and its handshake; only a context
 * switch, a new bundle or another screen rebuilds it.
 */
export default function ScreenHost({ projection, fallback, loadAsset, actions, ...callbacks }: { projection: ScreenPush; fallback: ReactNode; loadAsset?: AssetLoader; actions?: Pick<ScreenActions, "call" | "planner"> } & Pick<ComponentProps<typeof ScreenOutlet>, "renderFailure" | "onRefreshScope" | "onMounted" | "onToolInvoked" | "onRoute" | "onCommand" | "onNavigation" | "onMediaAttach" | "onScreenDrawer" | "presentation" | "drawerTarget" | "restoreDrawer" | "onRestoreDrawerConsumed" | "socialSources" | "socialVisit" | "readOnly" | "route" | "suppressLoadingOverlay" | "onStateChange">) {
  const href = useHref();
  // A canvas frame (it owns its route) never follows the board URL's back door.
  const native = !callbacks.route && new URLSearchParams(href.split("?")[1] ?? "").get("screen") === "native";
  return native ? fallback : <ScreenOutlet projection={projection} fallback={fallback} loadAsset={loadAsset} actions={actions} {...callbacks} />;
}

/** One generic installed screen, discovered only in a fresh verified installation
 * receipt. The fresh mint/consume still proves session, approval and live bytes;
 * a files entry or URL by itself never authorizes execution. */
export function InstalledScreenOutlet({ installation, contexts, contextId, admitted, fallback, ...callbacks }: {
  installation: Installation; contexts: AppContext[]; contextId: string; admitted: boolean; fallback: ReactNode;
} & Pick<ComponentProps<typeof ScreenOutlet>, "renderFailure" | "onRefreshScope" | "onRoute" | "onCommand" | "onNavigation" | "onMediaAttach" | "onScreenDrawer" | "presentation" | "drawerTarget" | "readOnly" | "route" | "suppressLoadingOverlay" | "onStateChange">) {
  const declarations = installation.files.filter(f => /^screens\/[a-z0-9][a-z0-9-]{0,31}\/screens\.json$/.test(f));
  const tag = declarations.length === 1 ? declarations[0].split("/")[1] : null;
  const liveContext = contexts.some(c => c.id === contextId && c.install_id === installation.install_id && c.state === "active");
  // CAD-2534: only legacy/non-records screens need snapshot polling and retained
  // tool extras; `screen-records.v1` mounts read directly via host services. Keyed
  // synchronously to `mountScope` so context/digest transitions flip back to "pending"
  // on the same render and never race with fast mount receipts.
  const mountScope = JSON.stringify([installation.install_id, installation.digest, tag, contextId, admitted]);
  const [mountedState, setMountedState] = useState<{ scope: string; kind: "records" | "legacy" } | null>(null);
  const mountKind = mountedState?.scope === mountScope ? mountedState.kind : "pending";
  const handleMounted = useCallback((info: { hostContract: string | null } | null) => {
    setMountedState(
      info === null
        ? null
        : { scope: mountScope, kind: info.hostContract === "screen-records.v1" ? "records" : "legacy" },
    );
  }, [mountScope]);
  const legacyPolling = admitted && tag !== null && mountKind === "legacy";
  const [snapshot, setSnapshot] = useState<WorkspaceAppSnapshot | null>(() => readWorkspaceAppSnapshot(installation.install_id));
  const scopedSnapshot = snapshot?.installation.install_id === installation.install_id ? snapshot : null;
  // Runs progress and reviewers approve out of band, so legacy screens follow
  // their scope: a fresh read now, another every five seconds while the tab is
  // visible, and one straight after an action.
  const activeRead = useRef<AbortController | null>(null);
  const reread = useRef<(fresh: boolean) => void>(() => undefined);
  useEffect(() => {
    // A viewer the frame would not admit (or a screen-records.v1 mount) is not
    // polled at all, and a superseded read cannot be restarted by a late action callback.
    reread.current = () => undefined;
    if (!legacyPolling) return;
    let live = true;
    const read = (fresh: boolean) => {
      if (!live) return;
      // A periodic tick leaves a read already in flight alone: the reader drops a
      // superseded call, so cancelling it on every tick would starve any read
      // slower than the interval and the screen would never gain its rows. The
      // native workspace skips an in-flight periodic refresh for the same reason,
      // and only supersedes it for a read that follows a write — which is what a
      // fresh call here is.
      if (activeRead.current && !fresh) return;
      activeRead.current?.abort();
      const controller = new AbortController();
      activeRead.current = controller;
      readWorkspaceAppSnapshotFresh(installation.install_id, controller.signal).then(
        (next) => { if (!controller.signal.aborted) setSnapshot(next); },
        () => undefined,
      ).finally(() => { if (activeRead.current === controller) activeRead.current = null; });
    };
    reread.current = read;
    read(true);
    const timer = window.setInterval(() => {
      if (document.visibilityState !== "hidden") read(false);
    }, 5000);
    return () => {
      live = false;
      reread.current = () => undefined;
      window.clearInterval(timer);
      activeRead.current?.abort();
    };
  }, [installation.install_id, legacyPolling]);
  // Reuse snapshot bindings instead of issuing a separate workspaceApps.bindings call.
  const bindings = useMemo(
    () =>
      scopedSnapshot
        ? contextId
          ? scopedSnapshot.bindings.filter((b) => b.context_id === contextId)
          : scopedSnapshot.bindings
        : [],
    [scopedSnapshot, contextId],
  );
  const runs = useMemo(
    () => (scopedSnapshot && Array.isArray(scopedSnapshot.runs) ? scopedSnapshot.runs : []),
    [scopedSnapshot],
  );
  // Approved text and the image loader come from the same bounded readers the
  // native workspace uses — the newest 32 runs, 600 characters each, read in
  // sequence and cached by artifact id — so the generic path inherits that
  // budget instead of fetching every artifact the scope holds.
  const { extras, loadAsset, reloadTools } = useScreenExtras({
    enabled: legacyPolling,
    installation,
    installId: installation.install_id,
    contextId: contextId || "",
    runs,
    bindings,
    workers: null,
    onDenied: () => undefined,
  });
  const projection = useMemo<ScreenPush>(() => {
    const empty: ScreenPush = {
      v: 2, op: "screen", install_id: installation.install_id, digest: installation.digest, tag: tag ?? "", context_id: contextId,
      installation: { install_id: installation.install_id, name: installation.name, title: installation.title, version: installation.version, digest: installation.digest, summary: installation.summary },
      contexts: contexts.filter(c => c.install_id === installation.install_id && c.state === "active").map(c => ({ id: c.id, label: c.config.label })),
      runs: [], outbox: [], publish_intents: { status: "ok", withheld: 0, rows: [] }, readiness: { ok: true, blockers: [] },
      actions: admitted && !callbacks.readOnly ? [...ACTION_VERBS] : [], now: Math.floor(Date.now() / 1000),
    };
    if (scopedSnapshot === null || tag === null) return empty;
    // Intent state is publish-intent state (social's concern) and stays absent
    // rather than guessed — the same state the native path settles to when its
    // own reads fail. Approved text arrives through `extras`.
    try {
      return screenProjection(scopedSnapshot.installation, tag, contextId, scopedSnapshot.contexts, runs, Array.isArray(scopedSnapshot.effects) ? scopedSnapshot.effects : [], undefined, extras) ?? empty;
    } catch {
      // An oversized or otherwise unavailable scope keeps today's behaviour.
      return empty;
    }
  }, [scopedSnapshot, runs, installation, contexts, contextId, tag, extras, admitted, callbacks.readOnly]);
  const actions = useMemo(() => {
    const ctx = () => ({ installation, installId: installation.install_id, contextId, runs, onChanged: () => reread.current(true) });
    return { call: (verb: Parameters<typeof runCall>[1], args: Parameters<typeof runCall>[2], ui: Parameters<typeof runCall>[3]) => runCall(ctx(), verb, args, ui), planner: makePlanner(ctx) };
  }, [installation, contextId, runs]);
  return admitted && liveContext && tag && installation.executable && installation.approval.state === "approved" && installation.removed == null
    ? <ScreenHost projection={projection} fallback={fallback} loadAsset={loadAsset} actions={callbacks.readOnly ? undefined : actions} onMounted={handleMounted} onToolInvoked={reloadTools} {...callbacks} /> : fallback;
}
