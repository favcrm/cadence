import { useContext, useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { api, ApiError, type MasterPermissionRequest } from "../../../lib/api";
import { useTranslate } from "../../../lib/locale";
import { useWriteBlock, WriteGate } from "../../auth/WriteGate";
import { publishPermission } from "../../home/PermissionCard";
import { cleanPermissionInline } from "../../home/permissionFormat";
import type { TurnBarState } from "./turnBarState";

/** Focuses the first actionable control inside a mounted needs-approval TurnBar. */
export function focusApprovalBar(container: ParentNode | null | undefined): boolean {
  const bar = container?.querySelector<HTMLElement>("[data-turn-bar][data-state='needs_approval']");
  if (!bar) return false;
  const target = bar.querySelector<HTMLElement>("button:not(:disabled)") ?? bar;
  target.focus();
  return true;
}

/** Focuses a composer textarea inside `container`, falling back to `document.getElementById`. */
export function focusComposerById(container: ParentNode | null | undefined, id: string): void {
  const box =
    container?.querySelector<HTMLTextAreaElement>(`#${id}`) ??
    (document.getElementById(id) as HTMLTextAreaElement | null);
  box?.focus();
}

export interface TurnBarActions {
  /** Mutating actions are absent when the viewer cannot write. */
  onCancel?: (message: string) => void;
  /** Absent when there is no captured envelope to resend: Retry is hidden. */
  onRetry?: () => void;
  onReview: () => void;
  /** Opens the exact queued predecessor: Home or an installed app's conversation. */
  onView?: () => void;
}

export interface TurnBarPermission {
  request: MasterPermissionRequest;
  readOnly: boolean;
  onRejectAndReply?: (request: MasterPermissionRequest) => void;
  onFocusComposer?: () => void;
}

export interface TurnBarProps {
  state: TurnBarState;
  actions: TurnBarActions;
  reconnecting: boolean;
  hideSending?: boolean;
  permission?: TurnBarPermission | null;
}

/**
 * One row above the composer for the conversation's current turn
 * (CAD-1700). Neutral surface; the state is in the dot and the words.
 * While the stream is down the bar shows the last known state, dimmed and
 * prefixed "last known", with its actions hidden. A finished turn renders
 * nothing: the reply itself is the completion signal.
 *
 * A running turn renders nothing either (CAD-1880): the assistant's typing
 * bubble at the transcript tail is the loading indicator, and Stop lives in
 * the composer. This row is left to the states that need words or an action
 * the bubble cannot carry — queued, sending, approval, failed, stopped.
 */
export function TurnBar({
  state,
  actions,
  reconnecting,
  hideSending = false,
  permission = null,
}: TurnBarProps) {
  const t = useTranslate();
  const gateBlock = useContext(WriteGate);
  const writeBlock = useWriteBlock(permission?.readOnly ?? true) ?? gateBlock;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [alwaysOpen, setAlwaysOpen] = useState(false);
  const [rejectOpen, setRejectOpen] = useState(false);
  const mountedRef = useRef(true);
  const requestSeq = useRef(0);
  const alwaysBtnRef = useRef<HTMLButtonElement>(null);
  const denyBtnRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    requestSeq.current += 1;
    setBusy(false);
    setError(null);
    setAlwaysOpen(false);
    setRejectOpen(false);
  }, [permission?.request.id, state.kind]);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      requestSeq.current += 1;
    };
  }, []);

  if (state.kind === "idle" || state.kind === "working" || state.kind === "writing") return null;
  // Hide the row, not the component: an in-flight permission decision can
  // still need its reply callback after the state changes to sending.
  if (hideSending && state.kind === "sending") return null;

  const canDecide =
    state.kind === "approval" &&
    !reconnecting &&
    permission !== null &&
    !permission.readOnly &&
    writeBlock === null;

  const runDecision = (work: () => Promise<unknown>, after?: () => void) => {
    if (!canDecide || busy) return;
    const seq = requestSeq.current;
    setBusy(true);
    setError(null);
    work()
      .then(() => {
        if (requestSeq.current === seq) {
          setAlwaysOpen(false);
          setRejectOpen(false);
        }
        publishPermission();
        if (mountedRef.current) after?.();
      })
      .catch((e: unknown) => {
        const msg = e instanceof ApiError ? e.message : e instanceof Error ? e.message : String(e);
        if (e instanceof ApiError && (e.status === 401 || e.status === 403)) {
          if (requestSeq.current === seq) {
            setAlwaysOpen(false);
            setRejectOpen(false);
            setError(msg);
          }
          // Re-evaluate pending permission state immediately on auth loss.
          publishPermission();
          return;
        }
        if (requestSeq.current !== seq) return;
        setError(msg);
      })
      .finally(() => {
        if (requestSeq.current === seq) setBusy(false);
      });
  };

  const button = (label: string, onClick: () => void, keyHint?: string) => (
    <button
      type="button"
      className="tb-btn"
      onClick={onClick}
      aria-label={keyHint ? t(label) : undefined}
      aria-keyshortcuts={keyHint}
    >
      {t(label)}
      {keyHint ? (
        <>
          {" "}
          <kbd className="tb-kbd" aria-hidden="true">{keyHint}</kbd>
        </>
      ) : null}
    </button>
  );
  const { onCancel } = actions;
  let title: string;
  let detail = "";
  let detailNode: ReactNode = null;
  let tone: string = state.kind;
  let controls: ReactNode = null;
  switch (state.kind) {
    case "sending":
      title = t("Sending");
      break;
    case "queued":
      title = t("Queued");
      detail = state.ahead === "" ? "" : `${t("after")} ${t(state.ahead)}`;
      controls = (
        <>
          {actions.onView ? button("View", actions.onView) : null}
          {state.cancelMessage !== null && onCancel ? button("Cancel", () => onCancel(state.cancelMessage!)) : null}
        </>
      );
      break;
    case "approval": {
      title = t("Needs your approval");
      tone = "needs_approval";
      if (permission !== null) {
        const req = permission.request;
        const cleanCmd = cleanPermissionInline(req.command);
        detailNode = (
          <span className="tb-detail tb-detail-perm">
            {reconnecting ? `${t("last known")} · ` : ""}
            <span className="chip bg-ink-800 text-ink-300 tb-risk" data-turn-risk={req.risk}>
              {t(req.risk)}
            </span>
            <code className="tb-cmd num" title={cleanCmd}>
              <bdi>{cleanCmd}</bdi>
            </code>
          </span>
        );
        controls = canDecide ? (
          <>
            <button
              type="button"
              className="tb-btn tb-btn-primary"
              disabled={busy}
              data-turn-allow-once
              aria-label={t("Allow once")}
              aria-keyshortcuts="1 y"
              onClick={() => runDecision(() => api.permissionAllowOnce(req.id))}
            >
              {t("Allow once")} <kbd className="tb-kbd" aria-hidden="true">1</kbd>
            </button>
            <button
              ref={alwaysBtnRef}
              type="button"
              className="tb-btn"
              disabled={busy}
              aria-expanded={alwaysOpen}
              data-turn-always-toggle
              onClick={() => {
                setRejectOpen(false);
                setAlwaysOpen((v) => !v);
              }}
            >
              {t("Always ▾")}
            </button>
            <button
              ref={denyBtnRef}
              type="button"
              className="tb-btn"
              disabled={busy}
              aria-expanded={rejectOpen}
              data-turn-deny-toggle
              onClick={() => {
                setAlwaysOpen(false);
                setRejectOpen((v) => !v);
              }}
            >
              {t("Deny ▾")}
            </button>
            {button("Review", actions.onReview, "r")}
          </>
        ) : (
          button("Review", actions.onReview, "r")
        );
      } else {
        detail = t("paused");
        controls = button("Review", actions.onReview);
      }
      break;
    }
    case "failed":
      title = t("Failed");
      detail = t(state.reason);
      controls = actions.onRetry ? button("Retry", actions.onRetry) : null;
      break;
    case "stopped":
      title = t("Stopped");
      detail = t("partial answer kept");
      break;
  }

  const onBarKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (state.kind !== "approval") return;
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    if (e.key === "Escape") {
      if (alwaysOpen || rejectOpen) {
        e.preventDefault();
        const restoreBtn = alwaysOpen ? alwaysBtnRef.current : denyBtnRef.current;
        setAlwaysOpen(false);
        setRejectOpen(false);
        restoreBtn?.focus();
        return;
      }
      if (permission?.onFocusComposer) {
        e.preventDefault();
        permission.onFocusComposer();
      }
      return;
    }
    if (e.shiftKey) return;
    if (e.key === "ArrowDown" && permission?.onFocusComposer) {
      e.preventDefault();
      permission.onFocusComposer();
      return;
    }
    if (!reconnecting && (e.key === "r" || e.key === "R")) {
      e.preventDefault();
      actions.onReview();
      return;
    }
    if (!canDecide || busy || permission === null) return;
    const req = permission.request;
    if (e.key === "1" || e.key === "y" || e.key === "Y") {
      e.preventDefault();
      runDecision(() => api.permissionAllowOnce(req.id));
    } else if (e.key === "2") {
      e.preventDefault();
      runDecision(() => api.permissionAlways(req.id, "exact", []));
    } else if (e.key === "3" && req.prefix && req.prefix.length > 0) {
      e.preventDefault();
      const prefix = req.prefix;
      runDecision(() => api.permissionAlways(req.id, "prefix", prefix));
    } else if (e.key === "n" || e.key === "N") {
      e.preventDefault();
      runDecision(() => api.permissionReject(req.id, false));
    }
  };

  return (
    <div
      className="turnbar"
      data-state={tone}
      data-stale={reconnecting || undefined}
      data-turn-bar
      role="status"
      aria-live="polite"
      onKeyDown={onBarKeyDown}
    >
      <span className="tb-dot" aria-hidden="true" />
      <span className="tb-text">
        <span className="tb-title">{title}</span>
        {detailNode ??
          ((detail || reconnecting) && (
            <span className="tb-detail">
              {reconnecting ? `${t("last known")} · ` : ""}
              {detail}
            </span>
          ))}
      </span>
      {!reconnecting && controls && <span className="tb-actions">{controls}</span>}
      {canDecide && alwaysOpen && permission !== null && (
        <div className="turnbar-sub" role="group" aria-label={t("always allow scope")} data-turn-always-menu>
          <button
            type="button"
            className="tb-btn"
            disabled={busy}
            data-turn-always-exact
            aria-label={t("This exact command")}
            aria-keyshortcuts="2"
            onClick={() => runDecision(() => api.permissionAlways(permission.request.id, "exact", []))}
          >
            {t("This exact command")} <kbd className="tb-kbd" aria-hidden="true">2</kbd>
          </button>
          {permission.request.prefix && permission.request.prefix.length > 0 ? (() => {
            const cleanPrefix = cleanPermissionInline(permission.request.prefix.join(" "));
            return (
              <button
                type="button"
                className="tb-btn"
                disabled={busy}
                data-turn-always-prefix
                aria-label={`${t("Prefix")} ${cleanPrefix}`}
                aria-keyshortcuts="3"
                onClick={() => {
                  const prefix = permission.request.prefix ?? [];
                  runDecision(() => api.permissionAlways(permission.request.id, "prefix", prefix));
                }}
              >
                {t("Prefix")} <bdi>{cleanPrefix}</bdi> <kbd className="tb-kbd" aria-hidden="true">3</kbd>
              </button>
            );
          })() : null}
        </div>
      )}
      {canDecide && rejectOpen && permission !== null && (
        <div className="turnbar-sub" role="group" aria-label={t("reject")} data-turn-deny-menu>
          <button
            type="button"
            className="tb-btn"
            disabled={busy}
            data-turn-reject-once
            aria-label={t("Reject once")}
            aria-keyshortcuts="n"
            onClick={() => runDecision(() => api.permissionReject(permission.request.id, false))}
          >
            {t("Reject once")} <kbd className="tb-kbd" aria-hidden="true">n</kbd>
          </button>
          {permission.onRejectAndReply ? (
            <button
              type="button"
              className="tb-btn"
              disabled={busy}
              data-turn-reject-reply
              onClick={() =>
                runDecision(
                  () => api.permissionReject(permission.request.id, false),
                  () => permission.onRejectAndReply?.(permission.request),
                )
              }
            >
              {t("Reject & reply…")}
            </button>
          ) : null}
          <button
            type="button"
            className="tb-btn"
            disabled={busy}
            data-turn-reject-never
            onClick={() => runDecision(() => api.permissionReject(permission.request.id, true))}
          >
            {t("Don't ask again")}
          </button>
        </div>
      )}
      {error && (
        <p className="turnbar-error text-micro text-fail break-words" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}
