import type { ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import { Bubble, USER_BUBBLE } from "../home/ThreadView";
import type { DeliveryState } from "./threadsClient";
import { deliveryLabel } from "./threadsView";

/** A delivery stage for display, as quiet metadata text (not a chip). Queued is never shown as delivered or seen. */
export function StageLabel({ state }: { state: DeliveryState | string }) {
  const t = useTranslate();
  return <span className="threads-stage" data-state={state}>{deliveryLabel(state, t)}</span>;
}

/** One transcript turn: the Home message bubble (Bubble, with the real sender
 *  and time). Operator turns take the Home outgoing tint and shape; agent and
 *  system turns stay plain. Compact density drops Home's 92% cap, so the body
 *  uses the full column. A `state` adds the delivery stage line. */
export function Turn({
  who,
  at,
  me,
  state,
  children,
}: {
  who: string;
  at?: string;
  me?: boolean;
  state?: DeliveryState | string | null;
  children: ReactNode;
}) {
  return (
    <Bubble who={who} at={at} me={me} density="compact">
      <div
        className={`inline-block text-left px-3 py-2 text-secondary text-ink-100 whitespace-pre-wrap [overflow-wrap:anywhere] ${
          me ? `bg-accent/10 border border-accent/20 ${USER_BUBBLE}` : "text-ink-200"
        }`}
      >
        {children}
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
