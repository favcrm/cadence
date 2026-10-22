import { useEffect, useState, type ReactNode } from "react";
import { useTranslate } from "../../../lib/locale";
import type { TurnBarState } from "./turnBarState";

export interface TurnBarActions {
  onStop: (message: string) => void;
  onCancel: (message: string) => void;
  /** Absent when there is no captured envelope to resend: Retry is hidden. */
  onRetry?: () => void;
  onReview: () => void;
  /** Opens what is ahead of a queued message (Home only says where it is). */
  onView?: () => void;
}

/**
 * One row above the composer for the conversation's current turn
 * (CAD-1700). Neutral surface; the state is in the dot and the words.
 * While the stream is down the bar shows the last known state, dashed,
 * with its actions hidden.
 */
export function TurnBar({
  state,
  since,
  actions,
  reconnecting,
}: {
  state: TurnBarState;
  /** Epoch seconds the running turn started (master_state.turn.since). */
  since: number | null;
  actions: TurnBarActions;
  reconnecting: boolean;
}) {
  const t = useTranslate();
  const live = state.kind === "working" || state.kind === "writing";
  const elapsed = useElapsed(live ? since : null);
  const faded = useFadedDone(state.kind === "done");
  if (state.kind === "idle" || faded) return null;

  const button = (label: string, onClick: () => void, danger = false, ariaLabel?: string) => (
    <button type="button" className={danger ? "tb-btn danger" : "tb-btn"} aria-label={ariaLabel} onClick={onClick}>
      {t(label)}
    </button>
  );
  const stop = (message: string | null) =>
    message === null ? null : button("Stop", () => actions.onStop(message), true, t("Stop this turn"));

  let title: string;
  let detail = "";
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
          {actions.onView && state.ahead !== "" ? button("View", actions.onView) : null}
          {state.cancelMessage !== null ? button("Cancel", () => actions.onCancel(state.cancelMessage!)) : null}
        </>
      );
      break;
    case "working":
      title = t("Working");
      detail = [state.step ?? t("planning"), clock(elapsed)].filter(Boolean).join(" · ");
      controls = stop(state.stopMessage);
      break;
    case "writing":
      title = t("Writing");
      detail = clock(elapsed);
      controls = stop(state.stopMessage);
      break;
    case "approval":
      title = t("Needs your approval");
      detail = t("paused");
      tone = "needs_approval";
      controls = button("Review", actions.onReview);
      break;
    case "failed":
      title = t("Failed");
      detail = state.reason;
      controls = actions.onRetry ? button("Retry", actions.onRetry) : null;
      break;
    case "stopped":
      title = t("Stopped");
      detail = t("partial answer kept");
      break;
    case "done":
      title = t("Done");
      detail = clock(state.seconds);
      break;
  }

  return (
    <div className="turnbar" data-state={tone} data-stale={reconnecting || undefined} data-turn-bar role="status" aria-live="polite">
      <span className="tb-dot" aria-hidden="true" />
      <span className="tb-text">
        <span className="tb-title">{title}</span>
        {(detail || reconnecting) && (
          <span className="tb-detail">
            {reconnecting ? `${t("last known")} · ` : ""}
            {detail}
          </span>
        )}
      </span>
      {!reconnecting && controls && <span className="tb-actions">{controls}</span>}
    </div>
  );
}

/** A finished turn's bar fades out after 3 s: a one-shot UI timer, not a poll. */
function useFadedDone(done: boolean): boolean {
  const [faded, setFaded] = useState(false);
  useEffect(() => {
    setFaded(false);
    if (!done) return;
    const timer = window.setTimeout(() => setFaded(true), 3000);
    return () => window.clearTimeout(timer);
  }, [done]);
  return done && faded;
}

/** Seconds since `since`, ticking once a second while a turn runs (a clock, not a fetch). */
function useElapsed(since: number | null): number | null {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (since === null) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [since]);
  return since === null ? null : Math.max(0, Math.floor(now / 1000 - since));
}

function clock(seconds: number | null): string {
  if (seconds === null) return "";
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}
