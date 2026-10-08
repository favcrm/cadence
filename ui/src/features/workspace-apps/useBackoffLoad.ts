import { useCallback, useEffect, useState } from "react";
import { loadFailureMessage, loadWithBackoff } from "./installationsLoad";

export interface BackoffLoad<T> {
  /** The last good answer; `null` until one arrives. Kept across failed refreshes. */
  data: T | null;
  /** The newest refresh failed (after any automatic busy retries). */
  error: string | null;
  /** A busy daemon is being retried automatically. */
  retrying: boolean;
  /** A request is in flight. */
  loading: boolean;
  retry: () => void;
}

/**
 * CAD-1189's load behaviour for any list (the same `loadWithBackoff` and
 * failure wording `useInstallations` uses): separate loading / error /
 * ready states, the last good answer kept across a failed refresh, a busy
 * daemon (503 `resource_busy`) retried 1/2/4 s, then a message with a
 * manual retry. An error never reads as an empty list.
 */
export function useBackoffLoad<T>(
  load: (signal: AbortSignal) => Promise<T>,
  refreshKey: unknown,
  enabled = true,
): BackoffLoad<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [retrying, setRetrying] = useState(false);
  const [loading, setLoading] = useState(false);
  const [manual, setManual] = useState(0);
  useEffect(() => {
    if (!enabled) return;
    const controller = new AbortController();
    setLoading(true);
    setRetrying(false);
    loadWithBackoff(() => load(controller.signal), {
      signal: controller.signal,
      onRetry: () => { if (!controller.signal.aborted) setRetrying(true); },
    }).then(
      (value) => {
        if (controller.signal.aborted) return;
        setData(value); setError(null); setRetrying(false); setLoading(false);
      },
      (cause: unknown) => {
        if (controller.signal.aborted) return;
        setError(loadFailureMessage(cause)); setRetrying(false); setLoading(false);
      },
    );
    return () => controller.abort();
    // `load` is a fresh closure each render; the key and retry drive refreshes.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, refreshKey, manual]);
  const retry = useCallback(() => setManual((n) => n + 1), []);
  return { data, error, retrying, loading, retry };
}
