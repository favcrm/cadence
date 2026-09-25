import type { ReactNode } from "react";
import type { Health } from "../lib/types";

/**
 * The header's right-hand status — read-only flag, the sign-in chip
 * (passed as children, between the flag and the daemon state), daemon
 * reachability, refresh, the write actor — as one chip row with two
 * renderings:
 *
 * - `header` (the sticky header): below `lg` the StatusChips labels turn
 *   sr-only and an icon carries each chip, so the row fits a phone and a
 *   screen reader still hears them; at `lg` the icons drop and the words
 *   return, unchanged. Chips passed as children keep their own shape at
 *   every width (SignIn already shrinks itself).
 * - `menu` (the <lg dropdown): icon and word together — touch has no
 *   hover for the header's `title`s, so the menu keeps each icon's
 *   meaning one tap away.
 */
export default function StatusChips({
  readOnly,
  mayWrite,
  health,
  actor,
  onRefresh,
  variant,
  children,
}: {
  /** The server refuses every write (meta.read_only) — the lock chip. */
  readOnly: boolean;
  /** This client may write — the "writes: actor" chip. */
  mayWrite: boolean;
  health: Health | null;
  actor: string;
  onRefresh: () => void;
  variant: "header" | "menu";
  children?: ReactNode;
}) {
  const header = variant === "header";
  const iconCls = header ? "lg:hidden" : undefined;
  const labelCls = header ? "max-lg:sr-only" : undefined;
  return (
    <>
      {readOnly && (
        <span
          className="chip bg-warn/10 text-warn"
          title="the server refuses every write — browsing only"
        >
          <svg
            width="12"
            height="12"
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinecap="round"
            aria-hidden="true"
            className={iconCls}
          >
            <rect x="3.2" y="7" width="9.6" height="6.8" rx="1.4" />
            <path d="M5.6 7V5.2a2.4 2.4 0 0 1 4.8 0V7" />
          </svg>
          <span className={labelCls}>read-only</span>
        </span>
      )}
      {children}
      {health && (
        <span
          className={`chip ${
            health.daemon === "reachable"
              ? "bg-ink-800 text-ink-400"
              : "bg-warn/10 text-warn"
          }`}
          title={
            health.daemon === "reachable"
              ? "daemon socket reachable"
              : "daemon socket unreachable — runtime strip is empty"
          }
        >
          {health.daemon === "reachable" ? (
            <svg
              width="12"
              height="12"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
              className={iconCls}
            >
              <path d="M1.8 8h2.6l2-4.6 3.2 9.2 2-4.6h2.6" />
            </svg>
          ) : (
            <svg
              width="12"
              height="12"
              viewBox="0 0 16 16"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
              className={iconCls}
            >
              <path d="M8 2.4 14.4 13.2H1.6L8 2.4z" />
              <path d="M8 6.6v3" />
              <circle cx="8" cy="11.4" r=".7" fill="currentColor" stroke="none" />
            </svg>
          )}
          <span className={labelCls}>daemon {health.daemon}</span>
        </span>
      )}
      <button
        onClick={onRefresh}
        className="chip bg-accent/10 text-accent hover:bg-accent/20 transition-colors"
        title="re-read the folders — writes also land here from the API"
      >
        <svg
          width="12"
          height="12"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
          className={iconCls}
        >
          <path d="M13.6 8a5.6 5.6 0 1 1-1.7-4" />
          <path d="M13.6 1.8v2.4h-2.4" />
        </svg>
        <span className={labelCls}>refresh</span>
      </button>
      {mayWrite && (
        <span
          className={`chip bg-ink-800 text-ink-400 ${header ? "hidden lg:inline-flex" : ""}`}
          title={`writes commit to the tracker as ${actor}`}
        >
          writes: {actor}
        </span>
      )}
    </>
  );
}
