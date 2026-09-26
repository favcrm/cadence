import type { ReactNode } from "react";
import type { Health } from "../lib/types";
import { IconLock, IconPulse, IconRefresh, IconWarning } from "./icons";

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
          <IconLock className={iconCls} />
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
            <IconPulse className={iconCls} />
          ) : (
            <IconWarning className={iconCls} />
          )}
          <span className={labelCls}>daemon {health.daemon}</span>
        </span>
      )}
      <button
        onClick={onRefresh}
        className="chip bg-accent/10 text-accent hover:bg-accent/20 transition-colors"
        title="re-read the folders — writes also land here from the API"
      >
        <IconRefresh className={iconCls} />
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
