import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ApiError } from "../../../lib/api";
import { workspaceApps, type AppBinding, type Installation, type WorkspaceRun } from "../workspaceApps";
import { latestSourceRun, type ScreenExtras } from "./screenProjection";
import { downscaleToDataUrl } from "./screenAssets";
import type { AssetLoader } from "./screenLifecycle";

/** Excerpts are read for at most this many newest runs per scope (R7). */
const EXCERPT_RUNS = 32;

/**
  * CAD-1123 HP1 — the board-held reads behind the screen.v2 projection:
 * the newest finished
 * read-slot run's results, and reviewer-approved caption text. Each read
 * is operator-only on the board and tagged with the scope it served; the
 * projection ignores a read for any other scope. Also the image loader
 * the screen channel calls for a pushed `image:<run id>` ref.
 */
export function useScreenExtras(args: {
  enabled: boolean; installation: Installation | null; installId: string; contextId: string;
  runs: WorkspaceRun[]; bindings: AppBinding[]; workers: number | null; onDenied: () => void;
}): { extras: ScreenExtras | undefined; loadAsset: AssetLoader } {
  const { enabled, installation, installId, contextId, runs, bindings, workers, onDenied } = args;
  const denied = useRef(onDenied);
  denied.current = onDenied;
  const refuse = (error: unknown) => {
    if (error instanceof ApiError && [401, 403].includes(error.status)) denied.current();
  };
  const scopedRuns = useMemo(() => runs.filter(run => run.install_id === installId &&
    (!contextId || run.context_id === contextId)), [runs, installId, contextId]);

  const scope = JSON.stringify([installId, contextId]);

  // The newest finished read-slot run's results; immutable once succeeded.
  const sourceRun = installation ? latestSourceRun(installation, scopedRuns)?.id ?? null : null;
  const [source, setSource] = useState<{ scope: string; value: ScreenExtras["source"] }>({ scope: "", value: undefined });
  useEffect(() => {
    if (!enabled || !sourceRun) return;
    const controller = new AbortController();
    workspaceApps.capabilityResults(sourceRun, controller.signal)
      .then(receipts => { if (!controller.signal.aborted) setSource({ scope, value: { runId: sourceRun, receipts } }); })
      .catch((error: unknown) => { if (!controller.signal.aborted) { refuse(error); setSource({ scope, value: null }); } });
    return () => controller.abort();
  }, [enabled, sourceRun, scope]);

  // Reviewer-approved caption text, cached by immutable artifact id.
  const texts = useRef(new Map<string, string>());
  const [textVersion, setTextVersion] = useState(0);
  const wanted = useMemo(() => [...scopedRuns].sort((a, b) => (b.created ?? 0) - (a.created ?? 0)).slice(0, EXCERPT_RUNS)
    .flatMap(run => {
      const approve = run.reviews.find(review => review.decision === "approve");
      const artifact = approve && run.artifacts.find(value => value.digest === approve.artifact_digest);
      return artifact && artifact.media_type.startsWith("text/") ? [artifact.id] : [];
    }).filter(id => !texts.current.has(id)), [scopedRuns, textVersion]);
  const wantedKey = wanted.join(",");
  useEffect(() => {
    if (!enabled || !wantedKey) return;
    const controller = new AbortController();
    void (async () => {
      for (const id of wantedKey.split(",")) {
        if (controller.signal.aborted) return;
        try {
          const artifact = await workspaceApps.artifact(id, controller.signal);
          if (artifact.id === id && typeof artifact.text === "string") texts.current.set(id, artifact.text.slice(0, 600));
        } catch (error) { refuse(error); return; }
      }
      if (!controller.signal.aborted) setTextVersion(value => value + 1);
    })();
    return () => controller.abort();
  }, [enabled, wantedKey]);

  const extras = useMemo<ScreenExtras | undefined>(() => enabled ? {
    installId, contextId, bindings, workers,
    source: sourceRun ? (source.scope === scope && (source.value === null || source.value?.runId === sourceRun) ? source.value : undefined) : null,
    texts: new Map(texts.current),
  } : undefined, [enabled, installId, contextId, bindings, workers, source, sourceRun, scope, textVersion]);

  // The pushed ref names a run; only a run in this exact scope with a
  // reviewer-pinned image resolves. The channel already refused any ref
  // it did not push.
  const current = useRef({ scopedRuns, installId });
  current.current = { scopedRuns, installId };
  const loadAsset = useCallback<AssetLoader>(async ref => {
    const runId = ref.startsWith("image:") ? ref.slice("image:".length) : "";
    const run = current.current.scopedRuns.find(value => value.id === runId && value.install_id === current.current.installId);
    const receipt = run?.reviews.find(review => review.decision === "approve" && review.asset_receipt_id)?.asset_receipt_id;
    if (!receipt) return null;
    const asset = await workspaceApps.capabilityAsset(receipt);
    if (asset.receipt_id !== receipt) return null;
    return downscaleToDataUrl(asset.base64, asset.media_type);
  }, []);
  return { extras, loadAsset };
}
