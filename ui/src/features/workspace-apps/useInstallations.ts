import { useCallback, useEffect, useState } from "react";
import { loadFailureMessage, loadWithBackoff } from "./installationsLoad";
import {
  APPS_CHANGED_EVENT,
  applyHostedLifecycleToList,
  appExplorer,
  dropInstallationsSnapshot,
  hostedLifecycleFromEvent,
  readInstallationsGuarded,
  readInstallationsSnapshot,
  type Installation,
} from "./workspaceApps";

export interface InstallationsState {
  /** The last good list; `null` until one answer has arrived. Kept across failed refreshes. */
  list: Installation[] | null;
  /** The newest refresh failed (after any automatic busy retries). */
  error: string | null;
  /** A busy daemon is being retried automatically. */
  retrying: boolean;
  /** A request is in flight. */
  loading: boolean;
  retry: () => void;
}

/**
 * The installed-app list as separate loading, error and ready states. A failed
 * refresh keeps the last good list; a busy daemon is retried with bounded
 * backoff before the error is shown. `refreshKey` re-reads on change.
 */
/** CAD-1537: a member's apps come from the member-readable `/api/app-home`
 *  rows. Only app identity, display and lifecycle fields needed by the
 *  launcher are projected; none of the operator's bundle details are read.
 */
async function memberRows(signal: AbortSignal, shared: boolean): Promise<Installation[]> {
  const home = await appExplorer.home(signal, shared ? "member" : undefined);
  if (!Array.isArray(home?.installations)) throw new Error("The server returned an invalid app list.");
  return home.installations.map((row) => ({
    install_id: row.install_id,
    name: row.name,
    title: row.title,
    removed: row.attention?.state === "removed" ? 1 : null,
    state: row.attention?.state === "removed" ? "removed"
      : row.attention?.state === "off" ? "disabled" : undefined,
    project_link: row.project ?? "",
    navigation: row.navigation ?? null,
    i18n: row.i18n ?? null,
    listing: row.listing ?? null,
  }) as unknown as Installation);
}

export function useInstallations(enabled: boolean, refreshKey: unknown, source: "list" | "home" = "list", shareOperatorRead = true, paused = false): InstallationsState {
  const authority = source === "home" ? "member" : shareOperatorRead ? "operator" : "viewer";
  const [loaded, setLoaded] = useState<{ authority: string; list: Installation[] | null }>(() => ({
    authority, list: enabled && !paused && authority === "operator" ? readInstallationsSnapshot() : null,
  }));
  const list = enabled && !paused && loaded.authority === authority ? loaded.list : null;
  const [error, setError] = useState<string | null>(null);
  const [retrying, setRetrying] = useState(false);
  const [loading, setLoading] = useState(false);
  const [manual, setManual] = useState(0);
  useEffect(() => {
    if (!enabled) {
      // Not the operator (any more): drop the last operator list and state.
      dropInstallationsSnapshot();
      setLoaded({ authority, list: null }); setError(null); setRetrying(false); setLoading(false);
      return;
    }
    if (paused) {
      // This consumer does not need a read, but still has authority. Do not
      // evict the snapshot used by other operator surfaces.
      setLoaded({ authority, list: null }); setError(null); setRetrying(false); setLoading(false);
      return;
    }
    if (authority === "operator") {
      setLoaded((prev) => ({ authority, list: prev.authority === authority ? prev.list ?? readInstallationsSnapshot() : readInstallationsSnapshot() }));
    }
    const onAppsChanged = (event: Event) => {
      const change = hostedLifecycleFromEvent(event);
      if (change) setLoaded((prev) => prev.authority === authority
        ? { authority, list: applyHostedLifecycleToList(prev.list, change) } : prev);
    };
    if (authority === "operator") window.addEventListener(APPS_CHANGED_EVENT, onAppsChanged);
    const controller = new AbortController();
    setLoading(true);
    setRetrying(false);
    loadWithBackoff(() => source === "home" ? memberRows(controller.signal, manual === 0) : readInstallationsGuarded(controller.signal, shareOperatorRead && manual === 0, authority === "operator"), {
      signal: controller.signal,
      onRetry: () => { if (!controller.signal.aborted) setRetrying(true); },
    }).then(
      (value) => {
        if (controller.signal.aborted) return;
        if (!Array.isArray(value)) { setError("The server returned an invalid app list."); }
        else { setLoaded({ authority, list: value }); setError(null); }
        setRetrying(false); setLoading(false);
      },
      (cause: unknown) => {
        if (controller.signal.aborted) return;
        setError(loadFailureMessage(cause)); setRetrying(false); setLoading(false);
      },
    );
    return () => {
      controller.abort();
      if (authority === "operator") window.removeEventListener(APPS_CHANGED_EVENT, onAppsChanged);
    };
  }, [enabled, paused, refreshKey, manual, source, shareOperatorRead, authority]);
  const retry = useCallback(() => setManual((n) => n + 1), []);
  return { list, error: enabled && !paused ? error : null, retrying: enabled && !paused && retrying, loading: enabled && !paused && loading, retry };
}
