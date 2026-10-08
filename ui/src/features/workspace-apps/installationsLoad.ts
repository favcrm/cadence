import { ApiError } from "../../lib/api";

/** Waits between automatic retries of a busy daemon: three tries, then a manual retry. */
export const BUSY_BACKOFF_MS: readonly number[] = [1000, 2000, 4000];

/** The daemon is busy (PM write lock held): transient, so worth retrying. A 503 `daemon_unavailable` is not busy. */
export function isBusyError(cause: unknown): boolean {
  return cause instanceof ApiError && (cause.code === "resource_busy");
}

/** What the operator reads when a list could not be loaded. */
export function loadFailureMessage(cause: unknown): string {
  if (isBusyError(cause)) return "The workspace is busy, so the app list could not be refreshed.";
  if (cause instanceof ApiError && cause.code === "daemon_unavailable") return "Daemon unreachable. The app list could not be refreshed.";
  return cause instanceof Error && cause.message ? cause.message : "The app list could not be loaded.";
}

function sleepUnlessAborted(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) return resolve();
    const done = () => { clearTimeout(timer); signal.removeEventListener("abort", done); resolve(); };
    const timer = setTimeout(done, ms);
    signal.addEventListener("abort", done);
  });
}

/**
 * Run `once`; while it fails as busy, wait the next backoff step and try again.
 * Any other failure, or a busy one after the last step, is thrown. `onRetry`
 * fires before each wait so the UI can say it is retrying. An aborted signal
 * stops the retries.
 */
export async function loadWithBackoff<T>(
  once: () => Promise<T>,
  opts: {
    signal: AbortSignal;
    delays?: readonly number[];
    sleep?: (ms: number, signal: AbortSignal) => Promise<void>;
    onRetry?: (attempt: number) => void;
  },
): Promise<T> {
  const delays = opts.delays ?? BUSY_BACKOFF_MS;
  const sleep = opts.sleep ?? sleepUnlessAborted;
  for (let attempt = 0; ; attempt++) {
    try {
      return await once();
    } catch (cause) {
      if (opts.signal.aborted || !isBusyError(cause) || attempt >= delays.length) throw cause;
      opts.onRetry?.(attempt + 1);
      await sleep(delays[attempt], opts.signal);
      if (opts.signal.aborted) throw cause;
    }
  }
}
