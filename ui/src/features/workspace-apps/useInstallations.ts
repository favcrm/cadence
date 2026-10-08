import { useCallback, useEffect, useState } from "react";
import { loadFailureMessage, loadWithBackoff } from "./installationsLoad";
import { workspaceApps, type Installation } from "./workspaceApps";

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
export function useInstallations(enabled: boolean, refreshKey: unknown): InstallationsState {
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
    loadWithBackoff(() => workspaceApps.installations(controller.signal), {
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
  }, [enabled, refreshKey, manual]);
  const retry = useCallback(() => setManual((n) => n + 1), []);
  return { list, error, retrying, loading, retry };
}
