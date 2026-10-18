import { useCallback, useEffect, useState } from "react";
import { loadFailureMessage, loadWithBackoff } from "./installationsLoad";
import { appExplorer, workspaceApps, type Installation } from "./workspaceApps";

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
 *  rows. Only the fields route resolution reads are carried — `install_id`,
 *  `name` (the key), `title`, `removed` and `project_link`; nothing else of
 *  the operator's `Installation` is present, so these rows serve app-key
 *  resolution and the menu only. */
async function memberRows(signal: AbortSignal): Promise<Installation[]> {
  const home = await appExplorer.home(signal);
  if (!Array.isArray(home?.installations)) throw new Error("The server returned an invalid app list.");
  return home.installations.map((row) => ({
    install_id: row.install_id,
    name: row.name,
    title: row.title,
    removed: row.attention?.state === "removed" ? 1 : null,
    project_link: row.project ?? "",
  }) as unknown as Installation);
}

export function useInstallations(enabled: boolean, refreshKey: unknown, source: "list" | "home" = "list"): InstallationsState {
  const [list, setList] = useState<Installation[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [retrying, setRetrying] = useState(false);
  const [loading, setLoading] = useState(false);
  const [manual, setManual] = useState(0);
  useEffect(() => {
    if (!enabled) {
      // Not the operator (any more): drop the last operator list and state.
      setList(null); setError(null); setRetrying(false); setLoading(false);
      return;
    }
    const controller = new AbortController();
    setLoading(true);
    setRetrying(false);
    loadWithBackoff(() => source === "home" ? memberRows(controller.signal) : workspaceApps.installations(controller.signal), {
      signal: controller.signal,
      onRetry: () => { if (!controller.signal.aborted) setRetrying(true); },
    }).then(
      (value) => {
        if (controller.signal.aborted) return;
        if (!Array.isArray(value)) { setError("The server returned an invalid app list."); }
        else { setList(value); setError(null); }
        setRetrying(false); setLoading(false);
      },
      (cause: unknown) => {
        if (controller.signal.aborted) return;
        setError(loadFailureMessage(cause)); setRetrying(false); setLoading(false);
      },
    );
    return () => controller.abort();
  }, [enabled, refreshKey, manual, source]);
  const retry = useCallback(() => setManual((n) => n + 1), []);
  return { list, error, retrying, loading, retry };
}
