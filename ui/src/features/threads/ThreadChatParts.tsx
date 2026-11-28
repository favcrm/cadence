import type { ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import Select from "../../ui/Select";
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
  inlineFormat,
  attachments,
  onOpenIssue,
  knownIssue,
  state,
  avatar,
  children,
}: {
  who: string;
  at?: string;
  me?: boolean;
  showName?: boolean;
  markdown?: boolean;
  /** Render safe inline Markdown & fenced code blocks for human channel messages (no raw HTML or remote images). */
  inlineFormat?: boolean;
  attachments?: ReactNode;
  /** CAD-2387: opens a known issue id in a Markdown body; absent keeps ids plain. */
  onOpenIssue?: (id: string) => void;
  knownIssue?: (id: string) => boolean;
  state?: DeliveryState | string | null;
  avatar?: ReactNode;
  children: ReactNode;
}) {
  const hasBody = typeof children !== "string" || children.trim() !== "" || !attachments;
  return (
    <Bubble who={who} at={at} me={me} showName={showName} avatar={avatar}>
      {hasBody && (
        <div
          className={`text-left text-secondary text-ink-100 [overflow-wrap:anywhere] ${
            me
              ? "chat-bubble"
              : markdown && typeof children === "string"
                ? "inline-block px-3 py-2 text-ink-200"
                : "inline-block px-3 py-2 text-ink-200 whitespace-pre-wrap"
          }`}
        >
          {(me || inlineFormat) && typeof children === "string" ? (
            <OperatorText text={readableAnswer(children)} />
          ) : markdown && typeof children === "string" ? (
            <div className="answer">
              <div className="answer-body issue-reader">
                <Md text={children} idTokens onOpen={onOpenIssue} knownIssue={knownIssue} />
              </div>
            </div>
          ) : (
            children
          )}
        </div>
      )}
      {attachments}
      {state ? (
        <div className="mt-1">
          <StageLabel state={state} />
        </div>
      ) : null}
    </Bubble>
  );
}

export type ChannelDeliveryStatus = "sending" | "sent" | "failed";

/** WhatsApp-style channel bubble (CAD-2892): distinct incoming surface + subtle
 *  border, asymmetric corners, optional sender grouping, Agent pill, and inline
 *  bottom-right timestamp + delivery status via CSS `display: flow-root`. */
export function ChannelTurn({
  who,
  at,
  me,
  senderKind,
  grouped = false,
  status,
  markdown,
  inlineFormat,
  attachments,
  after,
  onOpenIssue,
  knownIssue,
  avatar,
  children,
}: {
  who: string;
  at?: string;
  me?: boolean;
  senderKind?: "human" | "agent";
  grouped?: boolean;
  status?: ChannelDeliveryStatus;
  markdown?: boolean;
  inlineFormat?: boolean;
  attachments?: ReactNode;
  after?: ReactNode;
  onOpenIssue?: (id: string) => void;
  knownIssue?: (id: string) => boolean;
  avatar?: ReactNode;
  children: ReactNode;
}) {
  const t = useTranslate();
  const hasBody = typeof children !== "string" || children.trim() !== "" || !attachments;
  const isAgent = !me && senderKind === "agent";
  const statusGlyph =
    status === "sending" ? "◷" : status === "failed" ? "!" : status === "sent" ? "✓" : null;
  const statusLabel =
    status === "sending"
      ? t("Sending…")
      : status === "failed"
        ? t("Failed to send")
        : status === "sent"
          ? t("Sent")
          : undefined;

  return (
    <div
      className={`threads-wa-turn ${me ? "is-me" : "is-them"} ${isAgent ? "is-agent" : ""} ${grouped ? "is-grouped" : ""} ${status ? `is-${status}` : ""}`}
      data-chat-message={me ? "user" : isAgent ? "agent" : "human"}
    >
      {!me && (
        <div className="threads-wa-avatar" aria-hidden>
          {grouped ? null : avatar}
        </div>
      )}
      <div className="threads-wa-col">
        <div className={`threads-wa-bubble ${me ? "is-me" : "is-them"} ${isAgent ? "is-agent" : ""}`}>
          {!me && !grouped && (
            <div className="threads-wa-sender">
              <span className="threads-wa-sender-name">{who}</span>
              {isAgent && <span className="threads-wa-agent-pill">{t("Agent")}</span>}
            </div>
          )}
          {me && <span className="sr-only">{who}</span>}
          <div className="threads-wa-flow">
            {hasBody && (
              <div
                className={`threads-wa-body [overflow-wrap:anywhere] ${
                  me || inlineFormat
                    ? ""
                    : markdown && typeof children === "string"
                      ? "answer"
                      : "whitespace-pre-wrap"
                }`}
              >
                {(me || inlineFormat) && typeof children === "string" ? (
                  <OperatorText text={readableAnswer(children)} />
                ) : markdown && typeof children === "string" ? (
                  <div className="answer-body issue-reader">
                    <Md text={children} idTokens onOpen={onOpenIssue} knownIssue={knownIssue} />
                  </div>
                ) : (
                  children
                )}
              </div>
            )}
            {attachments}
            {(at || statusGlyph) && (
              <span className="threads-wa-meta num" title={statusLabel}>
                {at ? <span className="threads-wa-at">{at}</span> : null}
                {statusGlyph ? (
                  <span
                    className={`threads-wa-tick is-${status}`}
                    aria-label={statusLabel}
                  >
                    {statusGlyph}
                  </span>
                ) : null}
              </span>
            )}
          </div>
        </div>
        {after}
      </div>
    </div>
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
            <div className="threads-target">
              <Select
                searchable
                size="sm"
                value={target}
                onChange={(next) => onTarget?.(next)}
                options={targetOptions.map((o) => ({ value: o.id, label: o.label }))}
                placeholder={targetPlaceholder}
                aria-label={targetLabel}
              />
            </div>
          )}
        </div>
      )}
      {helper && <p className="threads-note">{helper}</p>}
      {children}
      {notice && <p className="threads-note" role="status">{notice}</p>}
    </div>
  );
}
