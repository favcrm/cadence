import { useId, type ReactNode } from "react";
import type { Health } from "../lib/types";
import { useTranslate } from "../lib/locale";
import { IconConnection, IconLock, IconRefresh, IconWarning } from "./icons";

/** Quiet icon controls in the header; the phone menu keeps readable labels. */
export default function StatusChips({
  readOnly,
  health,
  hosted = false,
  onRefresh,
  variant,
  children,
}: {
  /** The server refuses every write (meta.read_only) — the lock chip. */
  readOnly: boolean;
  health: Health | null | "unavailable";
  /** A hosted board uses the platform path; its daemon is not this board's. */
  hosted?: boolean;
  onRefresh: () => void;
  variant: "header" | "menu";
  children?: ReactNode;
}) {
  const t = useTranslate();
  const header = variant === "header";
  const statusId = useId();
  const refreshId = useId();
  const labelCls = header ? "sr-only" : undefined;
  const checking = health === null;
  const failed = health === "unavailable";
  const reachable = health !== null && health !== "unavailable" && health.daemon === "reachable";
  const connection = checking
    ? t("Checking connection…")
    : failed
      ? t("Connection unavailable — Refresh to retry")
      : reachable
        ? t("Daemon connected")
        : t("Daemon disconnected");
  // A hosted board has no local daemon to connect to. An answered health
  // probe still proves the board is up; only an unanswered probe is a
  // connection failure worth showing. Non-hosted boards keep the daemon
  // warning, and a hosted board with a reachable daemon still says so.
  const showDaemonState = !hosted || failed || reachable;
  return (
    <>
      {readOnly && (
        <span
          className={header ? "header-state cursor-default" : "chip bg-ink-800 text-ink-400"}
          title={t("the server refuses every write — browsing only")}
        >
          <IconLock />
          <span className={labelCls}>{t("Read only").toLowerCase()}</span>
        </span>
      )}
      {children}
      {header ? (
        <>
          {showDaemonState && (
            <span className="header-control-wrap">
              <span tabIndex={0} role="img" aria-label={connection} aria-describedby={statusId}
                className={`header-icon connection-icon ${checking ? "text-ink-500" : reachable ? "text-ink-400" : "text-warn"}`}>
                {!checking && !reachable ? <IconWarning size={14} /> : <IconConnection size={14} />}
                {reachable && <span className="connection-dot" aria-hidden />}
              </span>
              <span id={statusId} role="tooltip" className="header-tooltip">{connection}</span>
            </span>
          )}
          <span className="header-control-wrap">
            <button type="button" onClick={onRefresh} className="header-icon text-ink-400"
              aria-label={t("Refresh board")} aria-describedby={refreshId}>
              <IconRefresh size={14} />
            </button>
            <span id={refreshId} role="tooltip" className="header-tooltip">{t("Refresh board")}</span>
          </span>
        </>
      ) : (
        <>
          {!checking && showDaemonState && <span className={`chip ${reachable ? "bg-ink-800 text-ink-400" : "bg-warn/10 text-warn"}`}>
            {reachable ? <IconConnection /> : <IconWarning />}<span>{connection}</span>
          </span>}
          <button type="button" onClick={onRefresh} className="chip bg-accent/10 text-accent hover:bg-accent/20 transition-colors">
            <IconRefresh /><span>{t("Refresh board")}</span>
          </button>
        </>
      )}
    </>
  );
}
