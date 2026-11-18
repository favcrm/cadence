import { useEffect, useId, useMemo, useState, type ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import { AssistantFace, useAssistantActor } from "../assistant/AssistantIdentity";
import { displayName } from "../assistant/identity";
import type { ThreadRef } from "../../lib/types";
import Md, { healStreamingMarkdown } from "../../ui/Md";
import { attachMeta } from "../app-shell/chat/attach";
import { askLabel } from "../canvas/askRefs";
import HireProposalCard from "../agents/HireProposalCard";
import TicketProposalCard from "../agents/TicketProposalCard";
import ArtifactCard from "../canvas/ArtifactCard";
import ImageBlock from "../canvas/ImageBlock";
import VideoBlock from "../canvas/VideoBlock";
import { useImageConversation } from "../canvas/imageConversation";
import { hostedOrigin } from "../auth/hosted";
import AttachmentMedia, { openAttachmentInTab } from "./AttachmentMedia";
import { ThreadPermission } from "./PermissionCard";
import AgentForm from "./AgentForm";
import { ACTION_MARK } from "./formModel";
import { foldable, formReceipt, inlineTokens, lineCount, segments, type Inline } from "./operatorText";
import { elapsedSecs, errorReason, formatElapsed, resultCount, toolKey, toolLabel, toolSubject } from "./turnText";
import {
  isArtifactPutCall,
  isCardItem,
  stepSummary,
  toolSteps,
  type PendingAttachment,
  type ThreadEntry,
  type ThreadItem,
} from "./thread";
import { carriesSecret } from "../app-shell/chat/directive";
import { useConversationPreview } from "../app-shell/chat/conversationPreview";

/**
 * The one renderer for master-thread entries (CAD-1029). Home renders at
 * "full"; a narrower host (the app-shell chat pane) uses "compact", which
 * only swaps avatar size, body text, bubble width and the step indent.
 */
export type Density = "full" | "compact";

export function time(created: string | null | undefined): string {
  if (!created) return "";
  const d = new Date(created);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
}

/**
 * A run of tool calls and results (CAD-551): one collapsed row that
 * opens into the paired steps. A refusal reads "refused", not "error";
 * a call still running shows the pulse instead of a result. Opening and
 * closing animate through the grid-rows height trick (styles.css).
 */
/** The tool rows themselves, with no disclosure of their own (CAD-1881):
 *  standalone groups own the `steps-head`, a turn's trace lists them flat
 *  under its single head. A turn trace asks for newest-first so the row
 *  under "Reasoning" is the latest call, not the one that opened the turn. */
function ToolRows({ entries, newestFirst = false, raw = false }: { entries: ThreadEntry[]; newestFirst?: boolean; raw?: boolean }) {
  const t = useTranslate();
  const steps = useMemo(() => {
    const paired = toolSteps(entries);
    return newestFirst ? paired.slice().reverse() : paired;
  }, [entries, newestFirst]);
  return (
    <ul className="steps-inner">
      {steps.map((s, i) => (
        <li key={s.id ?? `s${s.call.seq}-${i}`} className="step-row">
          <span
            aria-hidden
            className={`step-mark ${
              !s.done
                ? "step-live"
                : s.refused
                  ? "text-warn"
                  : s.error
                    ? "text-fail"
                    : "text-ok"
            }`}
          >
            {s.call.kind === "tool_result" ? "←" : !s.done ? "◌" : s.refused ? "⊘" : s.error ? "✕" : "✓"}
          </span>
          {(() => {
            // CAD-2363: a known hosted tool reads as what it did and what it
            // acted on; the raw call stays one toggle (and the title) away.
            const label = !raw && s.call.kind === "tool_call" && !carriesSecret(s.summary) ? toolLabel(s.summary) : null;
            if (label === null) return <span className="num min-w-0 flex-1 truncate" title={s.summary}>{s.summary}</span>;
            const subject = toolSubject(s.call.text);
            return (
              <span className="step-what min-w-0 flex-1 truncate" title={s.summary}>
                {t(label)}
                {subject !== "" && <span className="step-subject"> · {subject}</span>}
              </span>
            );
          })()}
          {s.refused && <span className="step-tag text-warn shrink-0">{t("refused")}</span>}
          {s.error && !s.refused && <span className="step-tag text-fail shrink-0">{t("error")}</span>}
          {(() => {
            // CAD-2363: what a result reads as. A saved artifact (native
            // `cadence artifact put`, or the hosted `artifact_put`) and the raw
            // view or an unknown tool read exactly as before; a known tool's
            // error shows its reason, and its JSON result a count (a record
            // with no list shows nothing — its row says what it did).
            if (!s.result || s.call.kind !== "tool_call") return null;
            const text = s.result.text;
            const label = raw ? null : toolLabel(s.summary);
            let shown: string | null;
            if (!s.error && (isArtifactPutCall(s.call.text) || toolKey(s.summary) === "artifact_put")) shown = t("in the canvas");
            else if (label === null) shown = stepSummary(text);
            else if (s.error) shown = stepSummary(errorReason(text) ?? text);
            else if (/^\s*[{[]/.test(text)) {
              const count = resultCount(text);
              shown = count === null ? null : count === 1 ? t("1 result") : t("{n} results").replace("{n}", String(count));
            } else shown = stepSummary(text);
            if (shown === null) return null;
            return (
              <span className="num truncate max-w-[40%] text-ink-500" title={text}>
                {shown}
              </span>
            );
          })()}
        </li>
      ))}
    </ul>
  );
}

function StepsGroup({ entries, density = "full" }: { entries: ThreadEntry[]; density?: Density }) {
  const t = useTranslate();
  const [open, setOpen] = useState(false);
  const steps = useMemo(() => toolSteps(entries), [entries]);
  const refused = steps.filter((s) => s.refused).length;
  const failed = steps.filter((s) => s.error && !s.refused).length;
  const running = steps.some((s) => !s.done);
  const last = steps[steps.length - 1];
  return (
    <div className="steps min-w-0" data-kind="tools" data-density={density} data-open={open || undefined}>
      <button
        type="button"
        className="steps-head"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        <span className="steps-caret num shrink-0" aria-hidden>
          ›
        </span>
        <span className="shrink-0">
          {steps.length} {t("tool")} {steps.length === 1 ? t("step") : t("steps")}
        </span>
        {running && <span className="steps-running shrink-0">· {t("running")}</span>}
        {failed > 0 && (
          <span className="text-fail shrink-0">· {failed === 1 ? t("error") : `${failed} ${t("errors")}`}</span>
        )}
        {refused > 0 && (
          <span className="text-warn shrink-0">· {refused === 1 ? t("refused") : `${refused} ${t("refused")}`}</span>
        )}
        {last && (
          <span className="num truncate min-w-0 text-ink-500">
            · {(() => {
              const label = last.call.kind === "tool_call" && !carriesSecret(last.summary) ? toolLabel(last.summary) : null;
              return label !== null ? t(label) : last.summary;
            })()}
          </span>
        )}
      </button>
      <div className="steps-body">
        <ToolRows entries={entries} />
      </div>
    </div>
  );
}

/** CAD-1168 (approved mock): "Assistant 09:33" label above the body, no
 *  avatars. Home, the app panes, agent chats and Threads share it; density
 *  only changes the body text size. CAD-2362 (operator decision): the
 *  operator's own bubble carries no "You" row — its side and tint say who
 *  sent it — and its time sits under the bubble, shown on hover or focus
 *  (always on touch). `showName` keeps the row for an outgoing message that
 *  another person sent (a shared thread). `atAlways` keeps a delivery state
 *  ("sending…", "not sent") visible: it is information, not a timestamp. */
export function Bubble({
  who,
  at,
  children,
  me,
  live,
  showName,
  atAlways,
}: {
  who: string;
  at?: string;
  children: ReactNode;
  me?: boolean;
  /** CAD-1462: subtle right-aligned live state on the name row (the
   *  mock's `.who .live`) — the working indicator for the turn this
   *  row belongs to. */
  live?: ReactNode;
  showName?: boolean;
  atAlways?: boolean;
}) {
  if (me && !showName) {
    return (
      <div className="min-w-0 ml-6 text-right chat-me" data-chat-message="user">
        <span className="sr-only">{who}</span>
        <div className="min-w-0">{children}</div>
        {at ? (
          <div className="chat-me-at num" data-always={atAlways ? "" : undefined}>
            {at}
          </div>
        ) : null}
      </div>
    );
  }
  return (
    <div className={`min-w-0 ${me ? "ml-6 text-right" : ""}`} data-chat-message={me ? "user" : "assistant"}>
      <div className={`flex items-baseline gap-2 text-micro text-ink-500 mb-1.5 ${me ? "justify-end" : ""}`}>
        <span className="text-label font-medium text-ink-200">{who}</span>
        {at ? <span className="num">{at}</span> : null}
        {live ? <span className="chat-live num">{live}</span> : null}
      </div>
      <div className="min-w-0">{children}</div>
    </div>
  );
}

/** One inline token as React text — never markup (CAD-2362). */
function InlineToken({ token }: { token: Inline }) {
  switch (token.kind) {
    case "code":
      return <code className="op-code">{token.text}</code>;
    case "strong":
      return <strong>{token.text}</strong>;
    case "em":
      return <em>{token.text}</em>;
    case "link":
      return (
        <a className="lnk" href={token.href} target="_blank" rel="noopener noreferrer">
          {token.text}
        </a>
      );
    default:
      return <>{token.text}</>;
  }
}

/** Operator prose with its inline marks, line breaks as typed. */
function OperatorInline({ text }: { text: string }) {
  const tokens = useMemo(() => inlineTokens(text), [text]);
  return (
    <span className="whitespace-pre-wrap">
      {tokens.map((tok, i) => (
        <InlineToken key={i} token={tok} />
      ))}
    </span>
  );
}

/**
 * CAD-2362: the operator's own message on every chat surface — inline
 * Markdown, a pasted payload or fenced block as a code block, and a long
 * message folded after `FOLD_LINES` lines with its line count, Show all and
 * Copy. Copy takes the text as written (a form answer's readable lines).
 */
export function OperatorText({ text }: { text: string }) {
  const t = useTranslate();
  const parts = useMemo(() => segments(text), [text]);
  const fold = useMemo(() => foldable(parts), [parts]);
  const [open, setOpen] = useState(false);
  const bodyId = useId();
  const body = parts.map((s, i) =>
    s.kind === "code" ? (
      <pre key={i} className="op-pre num" data-lang={s.lang ?? undefined}>
        {s.text}
      </pre>
    ) : (
      <OperatorInline key={i} text={s.text} />
    ),
  );
  if (!fold) return <>{body}</>;
  const lines = lineCount(parts);
  return (
    <div className="op-fold" data-open={open ? "" : undefined}>
      {/* Focus landing on a clipped link opens the message, so it is never invisible. */}
      <div className="op-fold-body" id={bodyId} onFocus={() => setOpen(true)}>
        {body}
      </div>
      <div className="op-fold-foot">
        <span className="num">
          {parts.some((s) => s.kind === "code" && s.lang === "json") ? "JSON · " : ""}
          {lines} {t("lines")}
        </span>
        <span className="op-fold-actions">
          <button type="button" className="lnk" aria-expanded={open} aria-controls={bodyId} onClick={() => setOpen((o) => !o)}>
            {open ? t("Show less") : t("Show all")}
          </button>
          <CopyControl text={text} label={t("Copy message")} />
        </span>
      </div>
    </div>
  );
}

/** One thread item: bubble, commentary, tool steps, permission card or system line. */
export function ThreadItemView({
  item,
  readOnly,
  density = "full",
  onOpenIssue,
  onRetry,
  onDiscard,
  activity,
}: {
  item: ThreadItem;
  density?: Density;
  readOnly: boolean;
  onOpenIssue?: (id: string) => void;
  onRetry: (
    message: string,
    text: string,
    refs?: ThreadRef[],
    attachments?: PendingAttachment[],
    conversation?: string,
  ) => void;
  onDiscard: (message: string) => void;
  /** CAD-1462: the item's own header activity — rendered right-aligned
   *  on its bubble's name row. Only an `answer` bubble carries it (in-
   *  flight tools/commentary get a standalone name row instead). */
  activity?: ReactNode;
}) {
  const t = useTranslate();
  const actor = useAssistantActor();
  const compact = density === "compact";
  const bodyText = compact ? "text-secondary" : "text-body";
  switch (item.type) {
    case "operator": {
      const refs = entryRefs(item.entry.payload);
      const files = entryAttachments(item.entry.payload);
      // CAD-2362: an answer the form card shows (threadItems checked the card
      // reads it) is one receipt line; the stored text (the model's copy) is
      // unchanged. Any other answer keeps its bubble and the caption.
      const receipt = item.formShown ? formReceipt(item.entry.text) : null;
      if (receipt) {
        const at = time(item.entry.created);
        return (
          <div className="min-w-0 text-right" data-chat-message="user" data-form-receipt>
            <span className="form-receipt">
              <span className="sr-only">{t("You")}: </span>
              <span aria-hidden>↳ </span>
              {t("Answered with the form")}
              {receipt.map((line, i) => (
                <span key={i} className="form-receipt-line"> · {line}</span>
              ))}
              {at ? <span className="num text-ink-500"> · {at}</span> : null}
            </span>
          </div>
        );
      }
      return (
        <Bubble who={t("You")} at={time(item.entry.created)} me>
          <div className={`chat-bubble ${bodyText} text-ink-100`}>
            <OperatorText text={formAnswerText(item.entry.text, refs)} />
            <AttachmentRows files={files} />
          </div>
          {isFormAnswer(refs) && <div className="text-micro text-ink-500 mt-1">{t("Answered with the form")}</div>}
          <RefChips refs={refs} onOpenIssue={onOpenIssue} />
        </Bubble>
      );
    }
    case "pending": {
      // CAD-1485 round 3 (member surface only): a `delivery:
      // "unconfirmed"` pending row opted into the honest unknown-
      // delivery label — never "not sent", since the daemon may have
      // committed. Absent the marker every surface keeps the Master
      // "not sent" reading unchanged.
      const pendingAt =
        item.pending.state === "failed"
          ? item.pending.delivery === "unconfirmed"
            ? t("Delivery unconfirmed")
            : t("not sent")
          : item.pending.state === "sent"
            ? t("queued")
            : t("sending…");
      return (
        <Bubble
          who={t("You")}
          at={pendingAt}
          me
          atAlways
        >
          <div
            className={`chat-bubble ${bodyText} ${
              item.pending.state === "failed" ? "text-ink-200" : "text-ink-300"
            }`}
            data-pending={item.pending.state}
          >
            <OperatorText text={formAnswerText(item.pending.text, item.pending.refs ?? [])} />
            <AttachmentRows files={pendingAttachments(item.pending.attachments)} />
          </div>
          <RefChips refs={item.pending.refs ?? []} onOpenIssue={onOpenIssue} />
          {item.pending.state === "failed" && (
            <div className="text-micro text-fail mt-1 break-words">
              {item.pending.delivery === "unconfirmed" ? t("The send may still have been delivered — retry sends the same message once.") : item.pending.error}{" "}
              <button
                className="lnk"
                onClick={() =>
                  onRetry(
                    item.pending.message,
                    item.pending.text,
                    item.pending.refs,
                    item.pending.attachments,
                    item.pending.conversation,
                  )
                }
              >
                {t("Retry")}
              </button>{" "}
              ·{" "}
              <button className="lnk" onClick={() => onDiscard(item.pending.message)}>
                {t("Discard")}
              </button>
            </div>
          )}
        </Bubble>
      );
    }
    case "thinking":
      return <ThinkingNote entry={item.entry} />;
    case "commentary":
      return (
        <div
          className={`text-secondary text-ink-400 italic whitespace-pre-wrap break-words`}
          data-kind="commentary"
        >
          {item.entry.text}
        </div>
      );
    case "tools":
      return <StepsGroup entries={item.entries} density={density} />;
    case "artifact":
      return <ArtifactCard entry={item.entry} />;
    case "image":
      return <ImageBlock entry={item.entry} />;
    case "video":
      return <VideoBlock entry={item.entry} />;
    case "hire":
      return <HireProposalCard entry={item.entry} reason={item.reason} />;
    case "ticket":
      return <TicketProposalCard entry={item.entry} onOpenIssue={onOpenIssue} readOnly={readOnly} />;
    case "form":
      return <AgentForm entry={item.entry} status={item.status} />;
    case "answer": {
      // A failed turn carries its reason in payload.error and no text: say
      // why, instead of an empty bubble. No Retry here — the turn ran.
      const failure = turnFailure(item.entry, t);
      return (
        <Bubble who={displayName(actor.read, actor.scope, t)} at={time(item.entry.created)} live={activity}>
          {failure && (
            <div className={`answer-fail mt-0.5 ${bodyText} text-fail break-words`} role="alert" data-kind="turn-failure">
              {failure}
            </div>
          )}
          {item.entry.text.trim() !== "" && (
            <div className={`chat-bubble issue-reader ${bodyText} text-ink-200`} data-kind="answer">
              <Md text={item.entry.text} onOpen={onOpenIssue} />
            </div>
          )}
        </Bubble>
      );
    }
    case "system": {
      const e = item.entry;
      if (e.payload?.source === "permission") {
        return <ThreadPermission text={e.text} readOnly={readOnly} />;
      }
      // The bootstrap prompt lands as a system message — the daemon's
      // briefing for the master, hundreds of lines of markdown. It is
      // context for the turn, not chat: one collapsed note, GFM inside.
      // Anything multi-line gets the same treatment — a divider row
      // centres one line, never a wall of text.
      const briefing =
        e.payload?.source === "bootstrap" || e.message === "bootstrap-master";
      if (briefing || e.text.includes("\n") || e.text.length > 240) {
        return <SystemNote entry={e} briefing={briefing} onOpenIssue={onOpenIssue} />;
      }
      return (
        <div className="flex items-center gap-2 text-micro text-ink-500 min-w-0" data-kind="system">
          <span className="h-px flex-1 bg-ink-700" />
          <span className="min-w-0 max-w-[85%] whitespace-pre-wrap break-words text-center">
            {typeof item.entry.payload?.from === "string" ? `${item.entry.payload.from}: ` : ""}
            {item.entry.text}
          </span>
          <span className="h-px flex-1 bg-ink-700" />
        </div>
      );
    }
  }
}

/**
 * CAD-1782: the assistant's reasoning, collapsed. The answer follows as its
 * own row, so reasoning is context rather than part of the reply.
 */
function ThinkingNote({ entry }: { entry: ThreadEntry }) {
  const t = useTranslate();
  const [open, setOpen] = useState(false);
  // Guard 10 applies to what is shown, not only to what is interpreted: a
  // reasoning block carrying `confirm_token`/`request_id` is withheld whole,
  // in the app pane and on Home alike.
  const withheld = carriesSecret(entry.text);
  const shown = withheld ? t("Reasoning withheld — it contained a request token.") : entry.text;
  return (
    <div className="steps thinknote min-w-0" data-kind="thinking" data-open={open || undefined}>
      <button
        type="button"
        className="steps-head"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        <span className="steps-caret num shrink-0" aria-hidden>
          ›
        </span>
        <span className="shrink-0">{t("Thinking")}</span>
        <span className="num truncate min-w-0 text-ink-500">· {stepSummary(shown)}</span>
      </button>
      <div className="steps-body">
        <div className="steps-inner">
          <div
            className="text-secondary text-ink-400 italic whitespace-pre-wrap break-words min-w-0"
            data-thinking-body
          >
            {shown}
          </div>
        </div>
      </div>
    </div>
  );
}

/**
 * A system entry too long for the divider line (CAD-551 r2): the
 * session's bootstrap briefing or another multi-line note, closed by
 * default, opened into left-aligned GFM — never centred text.
 */
function SystemNote({
  entry,
  briefing,
  onOpenIssue,
}: {
  entry: ThreadEntry;
  briefing: boolean;
  onOpenIssue?: (id: string) => void;
}) {
  const t = useTranslate();
  const [open, setOpen] = useState(false);
  return (
    <div className="steps sysnote min-w-0" data-kind={briefing ? "briefing" : "system-note"} data-open={open || undefined}>
      <button
        type="button"
        className="steps-head"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        <span className="steps-caret num shrink-0" aria-hidden>
          ›
        </span>
        <span className="shrink-0">{briefing ? t("Session briefing") : t("Details")}</span>
        <span className="num truncate min-w-0 text-ink-500">· {stepSummary(entry.text)}</span>
      </button>
      <div className="steps-body">
        <div className="steps-inner">
          <div className="issue-reader text-secondary text-ink-300 break-words min-w-0" data-briefing-body>
            <Md text={entry.text} onOpen={onOpenIssue} />
          </div>
        </div>
      </div>
    </div>
  );
}

/** CAD-2285: an operator message citing a form is that form's answer. */
const isFormAnswer = (refs: ThreadRef[]): boolean => refs.some((r) => r.kind === "surface");
/** The answer's readable lines; the A2UI action after them is for the master. */
const formAnswerText = (text: string, refs: ThreadRef[]): string =>
  isFormAnswer(refs) ? text.split(ACTION_MARK)[0] : text;

/** The subjects an operator bubble cites (CAD-574 `refs`) — one chip
 *  each under the text, except a form answer's `surface` ref, which the
 *  bubble names instead. An `issue` ref is a real link to its page so a
 *  result sent with citations is one tap from the work it names
 *  (CAD-1574); other kinds stay a label. */
function RefChips({ refs: all, onOpenIssue }: { refs: ThreadRef[]; onOpenIssue?: (id: string) => void }) {
  const t = useTranslate();
  const refs = all.filter((r) => r.kind !== "surface");
  if (refs.length === 0) return null;
  return (
    <span className="refsrow" aria-label={t("cited rows")}>
      {refs.map((r) =>
        r.kind === "issue" && onOpenIssue ? (
          <span key={`${r.kind}:${r.id}`} className="refchip num" title={`${r.kind}:${r.id}`}>
            {r.kind}:
            <button type="button" className="lnk num" onClick={() => onOpenIssue(r.id)}>
              {r.id}
            </button>
          </span>
        ) : (
          <span key={`${r.kind}:${r.id}`} className="refchip num" title={`${r.kind}:${r.id}`}>
            {askLabel(r)}
          </span>
        ),
      )}
    </span>
  );
}

/** The reason a `turn_result` failed (its `payload.error`), or null when
 *  the turn did not fail. `billing_unavailable` names the owner's action;
 *  other reasons are shown as the daemon gave them. */
function turnFailure(entry: ThreadEntry, t: (english: string) => string): string | null {
  const p = entry.payload;
  const error = typeof p?.error === "string" ? p.error.trim() : "";
  if (p?.status !== "failed" && !error) return null;
  if (error.includes("billing_unavailable"))
    return t("Assistant could not run: billing is not set up for this company. The company owner needs to set up billing.");
  return error ? `${t("Assistant could not finish this turn")}: ${error}` : t("Assistant could not finish this turn.");
}

/** CAD-1168: `payload.attachments` as typed rows — a malformed value
 *  reads as none. The stored row carries only daemon-resolved metadata
 *  ({id,name,size,mime,sha256}); the UI never makes a path or URL of it. */
export interface EntryAttachment {
  id: string;
  name: string;
  size: number;
  mime: string;
}

export function entryAttachments(payload: unknown): EntryAttachment[] {
  const arr = (payload as { attachments?: unknown } | null)?.attachments;
  if (!Array.isArray(arr)) return [];
  return arr.filter(
    (a): a is EntryAttachment =>
      !!a &&
      typeof a === "object" &&
      typeof (a as EntryAttachment).id === "string" &&
      typeof (a as EntryAttachment).name === "string" &&
      typeof (a as EntryAttachment).size === "number" &&
      typeof (a as EntryAttachment).mime === "string",
  );
}

/** Optimistic attachment rows carried on a pending send while it awaits its
 *  stored `operator` entry. Entries that carry `{id}` only (for example a retry
 *  seeded from a legacy envelope) omit chips until the stored row arrives. */
export function pendingAttachments(attachments?: PendingAttachment[]): EntryAttachment[] {
  if (!Array.isArray(attachments)) return [];
  return attachments.filter(
    (a): a is EntryAttachment =>
      !!a &&
      typeof a.id === "string" &&
      typeof a.name === "string" &&
      typeof a.size === "number" &&
      typeof a.mime === "string",
  );
}

/** CAD-1462: past this many sent files the extras sit behind a +N more
 *  disclosure — the same collapse point the draft strip uses. */
const SENT_CHIP_LIMIT = 3;

/** The operator bubble's retained files as the mock's `.sentchips` row:
 *  compact read-only chips — extension tag, name, `size · kind` — that
 *  wrap beside the bubble and collapse past three behind a
 *  keyboard-accessible +N more. CAD-2219 adds the file itself above the
 *  row: an image thumbnail (lightbox on click) or an inline video, and a
 *  PDF chip opens the board's own inline read in a new tab. A file that
 *  the board cannot serve keeps the plain chip; no remove or retry
 *  affordance — these already sent. */
function AttachmentRows({ files }: { files: EntryAttachment[] }) {
  const t = useTranslate();
  const conversation = useImageConversation();
  // The attachment read exists on the hosted board only; a local board keeps
  // the chips exactly as they were.
  const hosted = hostedOrigin(typeof window === "undefined" ? null : window.location.hostname);
  const [expanded, setExpanded] = useState(false);
  if (files.length === 0) return null;
  const collapsed = !expanded && files.length > SENT_CHIP_LIMIT;
  const shown = collapsed ? files.slice(0, SENT_CHIP_LIMIT) : files;
  return (
    <>
      <AttachmentMedia files={files} conversation={conversation} hosted={hosted} />
      <div className="sentchips" data-entry-attachments>
        {shown.map((f) => {
          const title = `${f.name} · ${f.size} B · ${f.mime}`;
          const body = (
            <>
              <span className="sentchip-ext num" aria-hidden>
                {sentExt(f.name)}
              </span>
              <span className="sentchip-name">{f.name}</span>
              <span className="sentchip-meta num">{attachMeta(f.name, f.size, t)}</span>
            </>
          );
          return f.mime === "application/pdf" && hosted && conversation !== null ? (
            <button
              key={f.id}
              type="button"
              className="sentchip"
              title={`${title} — ${t("(opens in a new tab)")}`}
              onClick={() => openAttachmentInTab(f.id, conversation)}
            >
              {body}
            </button>
          ) : (
            <span key={f.id} className="sentchip" title={title}>
              {body}
            </span>
          );
        })}
        {collapsed && (
          <button
            type="button"
            className="lnk app-chat-more"
            aria-expanded="false"
            onClick={() => setExpanded(true)}
          >
            +{files.length - shown.length} {t("more")}
          </button>
        )}
        {expanded && files.length > SENT_CHIP_LIMIT && (
          <button
            type="button"
            className="lnk app-chat-more"
            aria-expanded="true"
            onClick={() => setExpanded(false)}
          >
            {t("Show less")}
          </button>
        )}
      </div>
    </>
  );
}

/** The chip's type tag — the uppercased extension, `FILE` when a name
 *  has none. */
function sentExt(name: string): string {
  const parts = name.split(".");
  const ext = parts.length > 1 ? parts[parts.length - 1].trim() : "";
  return ext ? ext.toUpperCase().slice(0, 5) : "FILE";
}

/** `payload.refs` as typed refs — a malformed value reads as none. */
export function entryRefs(payload: unknown): ThreadRef[] {
  const arr = (payload as { refs?: unknown } | null)?.refs;
  if (!Array.isArray(arr)) return [];
  return arr.filter(
    (r): r is ThreadRef =>
      !!r && typeof r === "object" &&
      typeof (r as ThreadRef).kind === "string" &&
      typeof (r as ThreadRef).id === "string",
  );
}

// ---- Home's one assistant turn -------------------------------------------
// The revamp groups the RENDER, not the data: `threadItems` stays flat for
// the canvas feed, the working state and the window, and `threadRuns` only
// says which contiguous items paint as one turn. A turn is one quiet head,
// one folded trace (reasoning / worker notes / tool runs), the answer on its
// own surface, then the artifact cards it produced.

/** An entry stamp (ISO string) as epoch ms, or null. */
function stampMs(created: string | null | undefined): number | null {
  if (!created) return null;
  const ms = Date.parse(created);
  return Number.isFinite(ms) ? ms : null;
}

/** A tool call's plain name for the closed trace: `catalog_search: website`
 *  reads as `Catalog search`. Only the first word is kept, so an argument
 *  cannot follow it in. Formats a known-shape string for reading; raw calls
 *  and their arguments stay in the open trace. */
function toolName(summary: string): string {
  if (carriesSecret(summary)) return "";
  const name = summary.trim().split(/[\s:]/, 1)[0].replaceAll("_", " ").replace(/\s+/g, " ").trim();
  return name.charAt(0).toUpperCase() + name.slice(1);
}

/** Plain names of a tool run's calls, newest first. A result whose call has
 *  been paged out is not named: its summary is the tool's output. */
function toolNames(entries: ThreadEntry[], t: (english: string) => string): string[] {
  const names: string[] = [];
  for (const step of toolSteps(entries)) {
    if (step.call.kind !== "tool_call") continue;
    const label = carriesSecret(step.summary) ? null : toolLabel(step.summary);
    const name = label !== null ? t(label) : toolName(step.summary);
    if (name !== "") names.unshift(name);
  }
  return names;
}

/** The line after the trace title: up to three distinct tools the turn used,
 *  newest first. Never note text and never arguments: both stay in the open
 *  trace, because a closed summary is on screen for anyone to read. */
function traceDigest(trace: ThreadItem[], t: (english: string) => string): { summary: string; errors: number } {
  let errors = 0;
  const names: string[] = [];
  for (const item of trace) {
    if (item.type !== "tools") continue;
    for (const step of toolSteps(item.entries)) {
      if (step.error) errors += 1;
    }
    names.unshift(...toolNames(item.entries, t));
  }
  return { summary: [...new Set(names)].slice(0, 3).join(", "), errors };
}

/** The tool the turn is on right now, when the newest trace row is a tool.
 *  A later reasoning note means the tool has finished, so the bubble goes
 *  back to "Thinking…" instead of naming a call that already returned. */
function runningTool(trace: ThreadItem[], t: (english: string) => string): string {
  for (let i = trace.length - 1; i >= 0; i--) {
    const item = trace[i];
    if (item.type === "tools") {
      const calls = toolSteps(item.entries).filter((s) => s.call.kind === "tool_call");
      const newest = calls[calls.length - 1];
      if (newest === undefined) continue;
      if (carriesSecret(newest.summary)) return "";
      // CAD-2363: a known tool reads as its own label ("Search the web…");
      // any other keeps "Running bash".
      const label = toolLabel(newest.summary);
      if (label !== null) return `${t(label)}…`;
      const name = toolName(newest.summary);
      return name === "" ? "" : `${t("Running")} ${name.charAt(0).toLowerCase()}${name.slice(1)}`;
    }
    if (item.type === "thinking" || item.type === "commentary") return "";
  }
  return "";
}

/** The newest stored stamp in a turn — its head's time when it has no answer. */
function lastCreated(items: ThreadItem[]): string | null {
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    if (item.type === "pending") continue;
    if (item.type === "tools") {
      const last = item.entries[item.entries.length - 1];
      if (last) return last.created ?? null;
      continue;
    }
    return item.entry.created ?? null;
  }
  return null;
}

/** Three typing dots plus the current step. The transport sends whole
 *  entries, never token deltas, so there is no skeleton and no fabricated
 *  answer body. The row is not its own live region: it mounts inside the
 *  thread's `role="log"` list, which already announces the addition. */
/** CAD-2411: the interim line is a note's first block as one run of prose,
 *  so a three-line clamp holds: heading, list and quote markers are
 *  dropped and line breaks become spaces. A note that opens with a code
 *  fence has no prose line; the dots stay alone. */
function interimLine(text: string): string | null {
  const first = text.trim().split(/\n\s*\n/)[0] ?? "";
  if (first.startsWith("```") || first.startsWith("~~~")) return null;
  const line = first
    .split("\n")
    .map((row) => row.trim().replace(/^(#{1,6}\s+|>\s?|[-*+]\s+|\d+[.)]\s+)/, ""))
    .filter((row) => row !== "")
    .join(" ");
  return line === "" ? null : line;
}

function TypingDots({ label }: { label: string }) {
  const t = useTranslate();
  const actor = useAssistantActor();
  return (
    <div className="typing" data-kind="typing">
      <span className="typing-dots" aria-hidden>
        <i />
        <i />
        <i />
      </span>
      <span className="typing-step">{label}</span>
      <span className="sr-only">{`${displayName(actor.read, actor.scope, t)} ${t("is typing")}`}</span>
    </div>
  );
}

/** One disclosure for the turn's evidence. Tool runs keep their own
 *  StepsGroup inside it, so raw detail is still behind the existing
 *  disclosure rather than printed in the main flow. */
function TurnTrace({
  items,
  steps,
  running,
  summary,
  errors,
  elapsed,
  failed,
  answered,
}: {
  items: ThreadItem[];
  steps: number;
  running: boolean;
  summary: string;
  errors: number;
  elapsed: string | null;
  /** The turn ended on a failure rather than an answer. */
  failed: boolean;
  /** This segment holds the answer. An earlier segment of a split turn does
   *  not, and may still be live, so it never claims to have finished. */
  answered: boolean;
}) {
  const t = useTranslate();
  const [open, setOpen] = useState(false);
  const [raw, setRaw] = useState(false);
  const id = useId();
  const hasTools = items.some((item) => item.type === "tools");
  const title = running
    ? t("Working")
    : failed
      ? t("Stopped")
      : elapsed
        ? `${t("Thought for")} ${elapsed}`
        : answered
          ? t("Finished")
          : t("Reasoning");
  return (
    <div className="steps turntrace min-w-0" data-kind="turn-trace" data-open={open || undefined}>
      <button
        type="button"
        className="steps-head turntrace-head"
        aria-expanded={open}
        aria-controls={id}
        onClick={() => setOpen((o) => !o)}
      >
        <span className="steps-caret num shrink-0" aria-hidden>
          ›
        </span>
        <span className="turntrace-title shrink-0">{title}</span>
        {summary !== "" && (
          <span className="turntrace-summary truncate min-w-0" title={summary}>
            · {summary}
          </span>
        )}
        <span className="turntrace-count num shrink-0" data-warn={errors > 0 || undefined}>
          {steps} {steps === 1 ? t("step") : t("steps")}
          {errors > 0 ? ` · ${errors} ${errors === 1 ? t("error") : t("errors")}` : ""}
        </span>
      </button>
      <div className="steps-body" id={id} inert={!open} aria-hidden={!open}>
        <div className="turntrace-wrap">
          <ol className="turntrace-list">
            {/* Newest first. A long run used to open on the first call, so
                the steps actually in progress sat below the fold. */}
            {[...items].reverse().map((item) => {
              if (item.type === "tools") {
                return (
                  <li key={item.key} className="turntrace-item" data-item-key={item.key}>
                    <ToolRows entries={item.entries} newestFirst raw={raw} />
                  </li>
                );
              }
              if (item.type !== "thinking" && item.type !== "commentary") return null;
              const withheld = item.type === "thinking" && carriesSecret(item.entry.text);
              return (
                <li key={item.key} className="turntrace-item" data-item-key={item.key}>
                  <div className="turntrace-kicker num">
                    {item.type === "thinking" ? t("Reasoning") : t("Worker note")}
                  </div>
                  <p className="turntrace-text">
                    {withheld ? t("Reasoning withheld — it contained a request token.") : item.entry.text}
                  </p>
                </li>
              );
            })}
          </ol>
          {hasTools && (
            <button type="button" className="lnk turntrace-raw" aria-pressed={raw} onClick={() => setRaw((r) => !r)}>
              {raw ? t("Hide raw calls") : t("Show raw calls")}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

/** CAD-2363: the running turn's one clock, beside the assistant's name. The
 *  tick is decorative (CAD-1419: no elapsed-second screen-reader chatter);
 *  the trace and the dock say the turn is working. */
function TurnClock({ since }: { since: number }) {
  const t = useTranslate();
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, []);
  return (
    <span className="turn-clock num" data-turn-clock aria-hidden>
      <span className="workdot live" />
      {formatElapsed((now - since) / 1000, t)}
    </span>
  );
}

/** The one Copy affordance for an answer, hover/focus-revealed in the turn
 *  head so it never sits on top of the answer text. With a `label` it is a
 *  plain always-visible link (a folded operator message's foot). */
function CopyControl({ text, label }: { text: string; label?: string }) {
  const t = useTranslate();
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className={label ? "lnk" : "lnk turn-copy"}
      aria-label={label ?? t("Copy answer")}
      onClick={() => {
        void navigator.clipboard
          ?.writeText(text)
          .then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1600);
          })
          .catch(() => undefined);
      }}
    >
      {copied ? t("Copied") : t("Copy")}
    </button>
  );
}

/** The answer surface: the existing Markdown renderer, on the turn's one
 *  tonal bubble. Tables, callouts and code chips are scoped styles on
 *  `.answer`; the Markdown itself is never forked. Copy lives in the turn
 *  head, so the bubble carries no chrome of its own. */
function AnswerSurface({
  entry,
  density,
  onOpenIssue,
  earlier = false,
}: {
  entry: ThreadEntry;
  density: Density;
  onOpenIssue?: (id: string) => void;
  /** An older transcript's last note, shown as its reply. */
  earlier?: boolean;
}) {
  const t = useTranslate();
  const failure = turnFailure(entry, t);
  return (
    <>
      {failure && (
        <div
          className="answer-fail text-fail break-words"
          role="alert"
          data-kind="turn-failure"
        >
          {failure}
        </div>
      )}
      {entry.text.trim() !== "" && (
        <section
          className="answer"
          data-kind="answer"
          data-earlier={earlier || undefined}
          aria-label={t("Assistant answer")}
        >
          {earlier && (
            <div className="answer-head">
              <span className="answer-earlier num">{t("Earlier transcript")}</span>
            </div>
          )}
          <div className={`answer-body issue-reader ${density === "compact" ? "text-secondary" : "text-body"}`}>
            <Md text={entry.text} onOpen={onOpenIssue} idTokens />
          </div>
        </section>
      )}
    </>
  );
}

/** Independent of visible entries: a denied empty page is not empty history. */
export function CloudContentNotice({ withheld }: { withheld: string | null | undefined }) {
  const t = useTranslate();
  return (
    <div role="status" className="text-label text-ink-400" data-kind="cloud-withheld">
      {withheld != null ? t("Cloud content unavailable") : null}
    </div>
  );
}

/**
 * Reconnecting (CAD-1782): one quiet line at the top of the thread. The last
 * saved turn stays readable exactly as it was — no stale answer border, no
 * relabelled card, no warning fill. Retry reopens the page's stream.
 */
export function StreamBanner({ onRetry }: { onRetry: () => void }) {
  const t = useTranslate();
  return (
    <div className="streambanner" role="status" data-kind="stale">
      <span className="banner-spinner" aria-hidden />
      <span>{t("Reconnecting — showing the last saved turn")}</span>
      <button type="button" className="lnk streambanner-retry" onClick={onRetry}>
        {t("Retry")}
      </button>
    </div>
  );
}

/**
 * One assistant turn on Home (the worker-transcript revamp): one quiet head,
 * one collapsed trace, one Markdown answer, then its artifact cards. The flat
 * items still exist for the canvas feed; this is only the paint.
 *
 * `running` comes from the host's turn state, never from the items: a turn
 * with no answer is not assumed to be streaming (an aborted turn is not).
 */
export function ThreadTurn({
  items,
  running = false,
  density = "full",
  allowLegacy = false,
  onOpenIssue,
  after,
  head = true,
  continued = false,
  copy = true,
  previewConversation = null,
  previewTransform,
  startedAt = null,
  clock = true,
}: {
  items: ThreadItem[];
  /** CAD-2363: when this turn started (ms) — the stamp of the row just before
   *  it, so a send queued behind another turn starts when that turn ended.
   *  The live clock counts from it and the finished "Thought for" measures
   *  from it, so the two agree. Absent: the trace's first entry. */
  startedAt?: number | null;
  /** False on a later segment of the same send (`clockRuns`): one clock per turn. */
  clock?: boolean;
  running?: boolean;
  density?: Density;
  /** The conversation whose live preview text this turn may paint, when the
   *  host has one. Absent (an app that never sends previews) changes nothing. */
  previewConversation?: string | null;
  /** Presentation rewrite for the live preview, exactly what this surface
   *  already applies to committed text (an app pane's id redaction). Home
   *  passes nothing and paints the server's text unchanged. */
  previewTransform?: (text: string) => string;
  /** Only a transcript with no `turn_result` anywhere may treat its last
   *  worker note as an older answer; a new transcript's intermediate note
   *  must stay in the trace. */
  allowLegacy?: boolean;
  onOpenIssue?: (id: string) => void;
  /** Plan cards and the correction report, under the answer. */
  after?: ReactNode;
  /** False for a continuation segment: one head per turn, on its first run. */
  head?: boolean;
  /** True for an earlier segment of a split turn; the answer lands on a later one. */
  continued?: boolean;
  /** False in a pane that copies from its header: refused directives stay
   *  inert, with no control in the row. */
  copy?: boolean;
}) {
  const t = useTranslate();
  const actor = useAssistantActor();
  const traceItems = items.filter(
    (item) => item.type === "thinking" || item.type === "commentary" || item.type === "tools",
  );
  const answer = items.find((item) => item.type === "answer");
  const artifacts = items.filter(isCardItem);
  // A transcript from before `turn_result` has no answer item: its last
  // worker note IS what the operator read as the reply. Only the LAST
  // non-artifact item can be that note — a note followed by tools is a
  // tool-turn step, not a reply, and must stay in the trace.
  const lastNote = [...items].reverse().find((item) => !isCardItem(item));
  const legacy =
    allowLegacy && !answer && !running && lastNote?.type === "commentary" ? lastNote : null;
  const trace = legacy ? traceItems.filter((item) => item.key !== legacy.key) : traceItems;
  const shown = answer ?? legacy;
  // Streaming is a turn the host says is running AND that has no answer yet:
  // a turn whose answer already landed keeps its finished look.
  const streaming = running && shown === null;
  // The assistant's actual in-flight words (CAD-2228), replaced frame by
  // frame. It paints only while this turn is still generating and has no
  // committed answer, and it is never presented as a stored reply.
  const preview = useConversationPreview(previewConversation, streaming);
  const previewText =
    preview === null || previewTransform === undefined ? preview : previewTransform(preview);
  const hasPreview = previewText !== null && previewText.trim() !== "";
  // CAD-2411: a step's words commit as a note and fold into the trace when
  // its tool call starts. Until the next step has live text, the latest note
  // stays where the preview was, so the words do not vanish between steps.
  const interim =
    streaming && !hasPreview
      ? [...trace].reverse().find((item) => item.type === "commentary")
      : undefined;
  const interimText =
    interim && !carriesSecret(interim.entry.text) ? interimLine(interim.entry.text) : null;
  const steps = trace.reduce(
    (count, item) => count + (item.type === "tools" ? toolSteps(item.entries).length : 1),
    0,
  );
  const intro = trace.find((item) => item.type === "thinking") ?? trace[0];
  const introEntry =
    intro === undefined ? undefined : intro.type === "tools" ? intro.entries[0] : intro.entry;
  const digest = traceDigest(trace, t);
  const startMs = startedAt ?? stampMs(introEntry?.created);
  const elapsedFor = streaming ? null : elapsedSecs(startMs, stampMs(shown?.entry.created));
  const elapsed = elapsedFor === null ? null : formatElapsed(elapsedFor, t);
  const headAt = shown?.entry.created ?? lastCreated(items);
  const tool = runningTool(trace, t);
  const typingLabel = tool !== "" ? tool : t("Thinking…");
  return (
    <div className="turn min-w-0" data-kind="turn" data-turn-state={streaming ? "running" : "complete"} data-turn-continued={continued || undefined}>
      {head && (
        <div className="turn-head">
          <AssistantFace size={16} />
          <span className="turn-name">{displayName(actor.read, actor.scope, t)}</span>
          {streaming && clock && startMs !== null ? (
            <TurnClock since={startMs} />
          ) : (
            <span className="turn-time num">{streaming ? t("now") : time(headAt)}</span>
          )}
          {copy && shown && shown.entry.text.trim() !== "" && <CopyControl text={shown.entry.text} />}
        </div>
      )}
      {trace.length > 0 && (
        <TurnTrace
          items={trace}
          steps={steps}
          running={streaming}
          summary={digest.summary}
          errors={digest.errors}
          elapsed={elapsed}
          failed={shown != null && turnFailure(shown.entry, t) !== null}
          answered={shown != null}
        />
      )}
      {/* Its name row and trace say the turn is live; this is the only
          place its own words appear before the committed answer replaces
          them. No timestamp, no copy, no stored row. */}
      {hasPreview && previewText !== null && (
        <div className="answer" data-kind="preview" aria-live="off">
          <div className={`answer-body issue-reader ${density === "compact" ? "text-secondary" : "text-body"}`}>
            <Md text={healStreamingMarkdown(previewText)} onOpen={onOpenIssue} idTokens />
          </div>
        </div>
      )}
      {interimText !== null && (
        <div className="answer turn-interim" data-kind="interim" aria-live="off">
          <div className={`answer-body issue-reader ${density === "compact" ? "text-secondary" : "text-body"}`}>
            <Md text={interimText} onOpen={onOpenIssue} idTokens />
          </div>
        </div>
      )}
      {/* Dots, then the current step. Under the trace once steps exist,
          so the disclosure names the turn and the bubble names what is
          running now. */}
      {streaming && (!hasPreview || tool !== "") && <TypingDots label={typingLabel} />}
      {shown && (
        <div data-item-key={shown.key}>
          <AnswerSurface entry={shown.entry} density={density} onOpenIssue={onOpenIssue} earlier={legacy !== null} />
        </div>
      )}
      {artifacts.length > 0 && (
        <div className="turn-artifacts">
          {artifacts.map((item) =>
            item.type === "form" ? (
              <div key={item.key} data-item-key={item.key}>
                <AgentForm entry={item.entry} status={item.status} />
              </div>
            ) : item.type === "artifact" || item.type === "image" || item.type === "video" ? (
              <div key={item.key} data-item-key={item.key} className={item.type === "artifact" ? undefined : "turn-image"}>
                {item.type === "image" ? <ImageBlock entry={item.entry} /> : item.type === "video" ? <VideoBlock entry={item.entry} /> : <ArtifactCard entry={item.entry} />}
              </div>
            ) : null,
          )}
        </div>
      )}
      {after}
    </div>
  );
}
