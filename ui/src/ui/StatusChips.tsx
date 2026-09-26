import { useId, type ReactNode } from "react";
import type { Health } from "../lib/types";
import { IconConnection, IconLock, IconRefresh, IconWarning } from "./icons";

/** Quiet icon controls in the header; the phone menu keeps readable labels. */
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
  const statusId = useId();
  const refreshId = useId();
  const labelCls = header ? "sr-only" : undefined;
  const reachable = health?.daemon === "reachable";
  const connection = !health ? "Checking connection…" : reachable ? "Daemon connected" : "Daemon disconnected — live activity is unavailable";
  return (
    <>
      {readOnly && (
        <span
          className="chip bg-warn/10 text-warn"
          title="the server refuses every write — browsing only"
        >
          <IconLock />
          <span className={labelCls}>read-only</span>
        </span>
      )}
      {children}
      {header ? (
        <>
          <span className="header-control-wrap">
            <span tabIndex={0} role="img" aria-label={connection} aria-describedby={statusId}
              className={`header-icon connection-icon ${!health ? "text-ink-500" : reachable ? "text-ink-400" : "text-warn"}`}>
              {health && !reachable ? <IconWarning size={16} /> : <IconConnection size={18} />}
              {reachable && <span className="connection-dot" aria-hidden />}
            </span>
            <span id={statusId} role="tooltip" className="header-tooltip">{connection}</span>
          </span>
          <span className="header-control-wrap">
            <button type="button" onClick={onRefresh} className="header-icon text-ink-400"
              aria-label="Refresh board" aria-describedby={refreshId}>
              <IconRefresh size={16} />
            </button>
            <span id={refreshId} role="tooltip" className="header-tooltip">Refresh board</span>
          </span>
        </>
      ) : (
        <>
          {health && <span className={`chip ${reachable ? "bg-ink-800 text-ink-400" : "bg-warn/10 text-warn"}`}>
            {reachable ? <IconConnection /> : <IconWarning />}<span>{connection}</span>
          </span>}
          <button type="button" onClick={onRefresh} className="chip bg-accent/10 text-accent hover:bg-accent/20 transition-colors">
            <IconRefresh /><span>Refresh board</span>
          </button>
        </>
      )}
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
