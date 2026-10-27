import type { ReactNode } from "react";
import { useTranslate } from "../../../lib/locale";
import type { TurnBarState } from "./turnBarState";

export interface TurnBarActions {
  /** Mutating actions are absent when the viewer cannot write. */
  onCancel?: (message: string) => void;
  /** Absent when there is no captured envelope to resend: Retry is hidden. */
  onRetry?: () => void;
  onReview: () => void;
  /** Opens the exact queued predecessor: Home or an installed app's conversation. */
  onView?: () => void;
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
}: {
  state: TurnBarState;
  actions: TurnBarActions;
  reconnecting: boolean;
}) {
  const t = useTranslate();
  if (state.kind === "idle" || state.kind === "working" || state.kind === "writing") return null;

  const button = (label: string, onClick: () => void) => (
    <button type="button" className="tb-btn" onClick={onClick}>
      {t(label)}
    </button>
  );
  const { onCancel } = actions;
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
          {actions.onView ? button("View", actions.onView) : null}
          {state.cancelMessage !== null && onCancel ? button("Cancel", () => onCancel(state.cancelMessage!)) : null}
        </>
      );
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
