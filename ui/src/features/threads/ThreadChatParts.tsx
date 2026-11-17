import type { ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import { Bubble, OperatorText } from "../home/ThreadView";
import { readableAnswer } from "../home/operatorText";
import Md from "../../ui/Md";
import type { DeliveryState } from "./threadsClient";
import { deliveryLabel } from "./threadsView";

/** A delivery stage for display, as quiet metadata text (not a chip). Queued is never shown as delivered or seen. */
export function StageLabel({ state }: { state: DeliveryState | string }) {
  const t = useTranslate();
  return <span className="threads-stage" data-state={state}>{deliveryLabel(state, t)}</span>;
}

/** One transcript turn: the Home message bubble (Bubble, with the real sender
 *  and time). Operator turns take the Home outgoing tint and shape, and a
 *  text body renders as Home's operator message (CAD-2362: inline Markdown,
 *  folded pastes); an agent or system text body (`markdown`) renders through
 *  the same `Md` and `.answer` styles as Home's answers, ID tokens included
 *  (CAD-2380). Other turns stay plain. `showName` keeps the
 *  sender row on an outgoing message another person sent. Compact density
 *  drops Home's 92% cap, so the body uses the full column. A `state` adds the
 *  delivery stage line. */
export function Turn({
  who,
  at,
  me,
  showName,
  markdown,
  state,
  children,
}: {
  who: string;
  at?: string;
  me?: boolean;
  showName?: boolean;
  markdown?: boolean;
  state?: DeliveryState | string | null;
  children: ReactNode;
}) {
  return (
    <Bubble who={who} at={at} me={me} showName={showName}>
      <div
        className={`text-left text-secondary text-ink-100 [overflow-wrap:anywhere] ${
          me
            ? "chat-bubble"
            : markdown && typeof children === "string"
              ? "inline-block px-3 py-2 text-ink-200"
              : "inline-block px-3 py-2 text-ink-200 whitespace-pre-wrap"
        }`}
      >
        {me && typeof children === "string" ? (
          <OperatorText text={readableAnswer(children)} />
        ) : markdown && typeof children === "string" ? (
          <div className="answer">
            <div className="answer-body issue-reader">
              <Md text={children} idTokens />
            </div>
          </div>
        ) : (
          children
        )}
      </div>
      {state ? (
        <div className="mt-1">
          <StageLabel state={state} />
        </div>
      ) : null}
    </Bubble>
  );
}

export type ComposerMode = "post" | "ask";

/** The shared frame above a composer: Post or Ask mode, the Ask target
 *  select, a helper line and a notice. Presentation only: the caller owns
 *  the mode, the target and every send path. `showModes` false drops the
 *  mode bar for a plain direct message — mode and target props unused. */
export function ThreadComposerFrame({
  showModes = true,
  modeLabel,
  allowAsk,
  mode = "post",
  onMode,
  targetLabel,
  target = "",
  onTarget,
  targetOptions = [],
  targetPlaceholder,
  helper,
  notice,
  children,
}: {
  showModes?: boolean;
  /** Group label for the mode bar; unused when `showModes` is false. */
  modeLabel?: string;
  allowAsk: boolean;
  mode?: ComposerMode;
  onMode?: (mode: ComposerMode) => void;
  targetLabel?: string;
  target?: string;
  onTarget?: (value: string) => void;
  targetOptions?: { id: string; label: string }[];
  targetPlaceholder?: string;
  helper?: string;
  notice?: string | null;
  children: ReactNode;
}) {
  const t = useTranslate();
  return (
    <div className="threads-composer threads-group">
      {showModes && (
        <div className="threads-mode" role="group" aria-label={modeLabel}>
          <button type="button" aria-pressed={mode === "post"} onClick={() => onMode?.("post")}>{t("Post")}</button>
          {allowAsk && (
            <button type="button" aria-pressed={mode === "ask"} onClick={() => onMode?.("ask")}>{t("Ask agent")}</button>
          )}
          {mode === "ask" && (
            <label className="threads-target">
              <span className="sr-only">{targetLabel}</span>
              <select value={target} onChange={(e) => onTarget?.(e.target.value)} aria-label={targetLabel}>
                <option value="">{targetPlaceholder}</option>
                {targetOptions.map((o) => (
                  <option key={o.id} value={o.id}>{o.label}</option>
                ))}
              </select>
            </label>
          )}
        </div>
      )}
      {helper && <p className="threads-note">{helper}</p>}
      {children}
      {notice && <p className="threads-note" role="status">{notice}</p>}
    </div>
  );
}
