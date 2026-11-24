import { useEffect, useRef } from "react";

export interface VisiblePollOptions {
  /** Call `onTick` once immediately when polling activates (while visible). */
  immediate?: boolean;
  /** Stop polling automatically after this many milliseconds. */
  maxDurationMs?: number;
  /** Also invoke `onTick` on window `focus` and `document` `visibilitychange` (default `true`). */
  onFocus?: boolean;
}

/**
 * CAD-2764: shared visibility-aware refresh hook for action cards waiting on an
 * external outcome (ShareRequestCard, ConnectCard). Runs `onTick` only while the
 * page is visible, listens for tab focus/visibility return by default, and
 * optionally caps total active duration via `maxDurationMs`.
 */
export function useVisiblePoll(
  active: boolean,
  intervalMs: number | null,
  onTick: () => void,
  options: VisiblePollOptions = {},
): void {
  const tickRef = useRef(onTick);
  tickRef.current = onTick;

  const immediate = options.immediate === true;
  const maxDurationMs = options.maxDurationMs;
  const onFocus = options.onFocus !== false;

  useEffect(() => {
    if (!active || typeof window === "undefined" || typeof document === "undefined") return;
    let stopped = false;
    let timer: ReturnType<typeof setInterval> | null = null;
    const deadline =
      maxDurationMs !== undefined && maxDurationMs > 0 ? Date.now() + maxDurationMs : null;

    const cleanup = () => {
      if (stopped) return;
      stopped = true;
      if (timer !== null) {
        clearInterval(timer);
        timer = null;
      }
      if (onFocus) {
        window.removeEventListener("focus", tick);
        document.removeEventListener("visibilitychange", tick);
      }
    };

    const tick = () => {
      if (stopped || document.visibilityState === "hidden") return;
      if (deadline !== null && Date.now() >= deadline) {
        cleanup();
        return;
      }
      tickRef.current();
    };

    if (onFocus) {
      window.addEventListener("focus", tick);
      document.addEventListener("visibilitychange", tick);
    }
    if (intervalMs !== null && intervalMs > 0) {
      timer = setInterval(tick, intervalMs);
    }
    if (immediate) tick();

    return cleanup;
  }, [active, intervalMs, immediate, maxDurationMs, onFocus]);
}
