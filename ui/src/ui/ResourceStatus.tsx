import type { ResourceState } from "../lib/cache";

function clock(ms: number | null): string {
  return ms === null ? "never" : new Date(ms).toLocaleTimeString();
}

/** Header chip while a refresh failed over the last good payload. */
export function StaleChip({ state }: { state: ResourceState<unknown> }) {
  if (state.status !== "stale") return null;
  return (
    <span
      className="chip bg-warn/10 text-warn"
      title={`the last refresh failed (${state.error ?? "unknown error"}) — showing data from ${clock(state.asOf)}`}
    >
      refresh failed — stale
    </span>
  );
}

/**
 * The block a view shows instead of its rows before anything loaded:
 * a load in progress, or a failure with nothing to fall back on. Returns
 * null once data exists (ok, empty or stale) — the view renders it.
 */
export function ResourceGate({
  state,
  loading,
  failed,
  onRetry,
}: {
  state: ResourceState<unknown>;
  /** What is loading, e.g. "loading issues…". */
  loading: string;
  /** What could not load, e.g. "could not load issues". */
  failed: string;
  onRetry?: () => void;
}) {
  if (state.status === "loading") {
    return (
      <div className="card mb-4 px-4 py-5 text-secondary text-ink-400" role="status">
        {loading}
      </div>
    );
  }
  if (state.status === "failed") {
    return (
      <div
        className="card mb-4 px-4 py-5 text-secondary text-fail border-fail/40 flex flex-wrap items-center gap-3"
        role="alert"
      >
        <span className="min-w-0">
          {failed} — {state.error ?? "unknown error"}
        </span>
        {onRetry && (
          <button
            onClick={onRetry}
            className="chip bg-fail/10 text-fail hover:bg-fail/20 transition-colors ml-auto"
          >
            retry
          </button>
        )}
      </div>
    );
  }
  return null;
}
