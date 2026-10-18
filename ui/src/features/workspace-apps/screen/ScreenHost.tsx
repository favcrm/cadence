import { useEffect, useMemo, useRef, useState, type ComponentProps, type ReactNode } from "react";
import { readWorkspaceAppSnapshot, readWorkspaceAppSnapshotFresh, workspaceApps, type Installation, type AppContext, type AppBinding, type WorkspaceAppSnapshot } from "../workspaceApps";
import { runCall, makePlanner } from "./screenActions";
import { useHref } from "../../../lib/useLocation";
import ScreenOutlet from "./ScreenOutlet";
import { screenProjection } from "./screenProjection";
import { useScreenExtras } from "./useScreenExtras";
import type { AssetLoader, ScreenActions } from "./screenLifecycle";
import type { ScreenPush } from "./screenProtocol";

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
export default function ScreenHost({ projection, fallback, loadAsset, actions, ...callbacks }: { projection: ScreenPush; fallback: ReactNode; loadAsset?: AssetLoader; actions?: Pick<ScreenActions, "call" | "planner"> } & Pick<ComponentProps<typeof ScreenOutlet>, "onRoute" | "onCommand" | "onNavigation" | "onMediaAttach">) {
  const href = useHref();
  const native = new URLSearchParams(href.split("?")[1] ?? "").get("screen") === "native";
  return native ? fallback : <ScreenOutlet projection={projection} fallback={fallback} loadAsset={loadAsset} actions={actions} {...callbacks} />;
}

/** One generic installed screen, discovered only in a fresh verified installation
 * receipt. The fresh mint/consume still proves session, approval and live bytes;
 * a files entry or URL by itself never authorizes execution. */
export function InstalledScreenOutlet({ installation, contexts, contextId, admitted, fallback, ...callbacks }: {
  installation: Installation; contexts: AppContext[]; contextId: string; admitted: boolean; fallback: ReactNode;
} & Pick<ComponentProps<typeof ScreenOutlet>, "onRoute" | "onCommand" | "onNavigation" | "onMediaAttach">) {
  const declarations = installation.files.filter(f => /^screens\/[a-z0-9][a-z0-9-]{0,31}\/screens\.json$/.test(f));
  const tag = declarations.length === 1 ? declarations[0].split("/")[1] : null;
  const liveContext = contexts.some(c => c.id === contextId && c.install_id === installation.install_id && c.state === "active");
  // The scope's own rows, read exactly as the native workspace reads them, so an
  // app screen is handed the same projection instead of a stubbed one. Until the
  // snapshot arrives the previous empty projection stands: mounting behaviour is
  // unchanged, it simply gains rows once they are known.
  const [snapshot, setSnapshot] = useState<WorkspaceAppSnapshot | null>(() => readWorkspaceAppSnapshot(installation.install_id));
  // Runs progress and reviewers approve out of band, so the screen follows its
  // scope the way the native workspace does: a fresh read now, another every
  // five seconds while the tab is visible, and one straight after an action.
  const reread = useRef<() => void>(() => undefined);
  useEffect(() => {
    let controller = new AbortController();
    const read = () => {
      controller.abort();
      controller = new AbortController();
      readWorkspaceAppSnapshotFresh(installation.install_id, controller.signal).then(
        (next) => { if (!controller.signal.aborted) setSnapshot(next); },
        () => undefined,
      );
    };
    reread.current = read;
    read();
    const timer = window.setInterval(() => {
      if (document.visibilityState !== "hidden") read();
    }, 5000);
    return () => { window.clearInterval(timer); controller.abort(); };
  }, [installation.install_id]);
  // Approved text for the pushed runs, resolved the way `read.run` resolves it: the
  // run's reviewer-approved text artifact, read by id. Bindings come from the same
  // read the native path uses; `workers` stays null — "unknown", as the type states —
  // rather than asserted, so nothing here claims a scope the host did not state.
  const [bindings, setBindings] = useState<AppBinding[]>([]);
  useEffect(() => {
    if (!admitted) return;
    const controller = new AbortController();
    workspaceApps.bindings(installation.install_id, contextId || undefined, controller.signal)
      .then((rows) => { if (!controller.signal.aborted) setBindings(rows); }, () => undefined);
    return () => controller.abort();
  }, [installation.install_id, contextId, admitted]);
  // Approved text and the image loader come from the same bounded readers the
  // native workspace uses — the newest 32 runs, 600 characters each, read in
  // sequence and cached by artifact id — so the generic path inherits that
  // budget instead of fetching every artifact the scope holds.
  const { extras, loadAsset } = useScreenExtras({
    enabled: admitted,
    installation,
    installId: installation.install_id,
    contextId: contextId || "",
    runs: Array.isArray(snapshot?.runs) ? snapshot.runs : [],
    bindings,
    workers: null,
    onDenied: () => undefined,
  });
  const projection = useMemo<ScreenPush>(() => {
    const empty: ScreenPush = {
      v: 2, op: "screen", install_id: installation.install_id, digest: installation.digest, tag: tag ?? "", context_id: contextId,
      installation: { install_id: installation.install_id, name: installation.name, title: installation.title, version: installation.version, digest: installation.digest, summary: installation.summary },
      contexts: contexts.filter(c => c.install_id === installation.install_id && c.state === "active").map(c => ({ id: c.id, label: c.config.label })),
      runs: [], outbox: [], publish_intents: { status: "ok", withheld: 0, rows: [] }, readiness: { ok: true, blockers: [] }, actions: [], now: Math.floor(Date.now() / 1000),
    };
    if (snapshot === null || tag === null) return empty;
    // Intent state is publish-intent state (social's concern) and stays absent
    // rather than guessed — the same state the native path settles to when its
    // own reads fail. Approved text arrives through `extras`.
    try {
      return screenProjection(snapshot.installation, tag, contextId, snapshot.contexts, Array.isArray(snapshot.runs) ? snapshot.runs : [], Array.isArray(snapshot.effects) ? snapshot.effects : [], undefined, extras) ?? empty;
    } catch {
      // An oversized or otherwise unavailable scope keeps today's behaviour.
      return empty;
    }
  }, [snapshot, installation, contexts, contextId, tag, extras]);
  const actions = useMemo(() => {
    const ctx = () => ({ installation, installId: installation.install_id, contextId, runs: snapshot?.runs ?? [], onChanged: () => reread.current() });
    return { call: (verb: Parameters<typeof runCall>[1], args: Parameters<typeof runCall>[2], ui: Parameters<typeof runCall>[3]) => runCall(ctx(), verb, args, ui), planner: makePlanner(ctx) };
  }, [installation, contextId, snapshot]);
  return admitted && liveContext && tag && installation.executable && installation.approval.state === "approved" && installation.removed == null
    ? <ScreenHost projection={projection} fallback={fallback} loadAsset={loadAsset} actions={actions} {...callbacks} /> : fallback;
}
