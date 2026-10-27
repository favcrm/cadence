import { useId, useMemo, useState, type ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import { AssistantFace, useAssistantActor } from "../assistant/AssistantIdentity";
import { displayName } from "../assistant/identity";
import type { ThreadRef } from "../../lib/types";
import Md from "../../ui/Md";
import { attachMeta } from "../app-shell/chat/attach";
import { askLabel } from "../canvas/askRefs";
import HireProposalCard from "../agents/HireProposalCard";
import ArtifactCard from "../canvas/ArtifactCard";
import { ThreadPermission } from "./PermissionCard";
import { isArtifactPutCall, stepSummary, toolSteps, type ThreadEntry, type ThreadItem } from "./thread";
import { carriesSecret } from "../app-shell/chat/directive";

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
 *  under its single 44px head. */
function ToolRows({ entries }: { entries: ThreadEntry[] }) {
  const t = useTranslate();
  const steps = useMemo(() => toolSteps(entries), [entries]);
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
          <span className="num min-w-0 break-all flex-1">{s.summary}</span>
          {s.refused && <span className="step-tag text-warn shrink-0">{t("refused")}</span>}
          {s.error && !s.refused && <span className="step-tag text-fail shrink-0">{t("error")}</span>}
          {s.result && s.call.kind === "tool_call" && (
            <span className="num truncate max-w-[40%] text-ink-500" title={s.result.text}>
              {!s.error && isArtifactPutCall(s.call.text) ? t("in the canvas") : stepSummary(s.result.text)}
            </span>
          )}
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
        {last && <span className="num truncate min-w-0 text-ink-500">· {last.summary}</span>}
      </button>
      <div className="steps-body">
        <ToolRows entries={entries} />
      </div>
    </div>
  );
}

/** CAD-1168 (approved mock): "You 09:32" / "Assistant 09:33" label above
 *  the body, no avatars. Home and the app panes share it; density only
 *  changes the body text size. */
export function Bubble({
  who,
  at,
  children,
  me,
  live,
}: {
  who: string;
  at?: string;
  children: ReactNode;
  me?: boolean;
  /** CAD-1462: subtle right-aligned live state on the name row (the
   *  mock's `.who .live`) — the working indicator for the turn this
   *  row belongs to. */
  live?: ReactNode;
}) {
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
    attachments?: { id: string }[],
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
    case "operator":
      return (
        <Bubble who={t("You")} at={time(item.entry.created)} me>
          <div className={`chat-bubble ${bodyText} text-ink-100 whitespace-pre-wrap`}>
            <span>{item.entry.text}</span>
            <AttachmentRows files={entryAttachments(item.entry.payload)} />
          </div>
          <RefChips refs={entryRefs(item.entry.payload)} onOpenIssue={onOpenIssue} />
        </Bubble>
      );
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
        >
          <div
            className={`chat-bubble ${bodyText} whitespace-pre-wrap ${
              item.pending.state === "failed" ? "text-ink-200" : "text-ink-300"
            }`}
            data-pending={item.pending.state}
          >
            {item.pending.text}
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
    case "hire":
      return <HireProposalCard entry={item.entry} reason={item.reason} />;
    case "answer": {
      // A failed turn carries its reason in payload.error and no text: say
      // why, instead of an empty bubble. No Retry here — the turn ran.
      const failure = turnFailure(item.entry, t);
      return (
        <Bubble who={displayName(actor.read, actor.scope, t)} at={time(item.entry.created)} live={activity}>
          {failure && (
            <div className={`mt-0.5 ${bodyText} text-fail break-words`} role="alert" data-kind="turn-failure">
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

/** The subjects an operator bubble cites (CAD-574 `refs`) — one chip
 *  each under the text. An `issue` ref is a real link to its page so a
 *  result sent with citations is one tap from the work it names
 *  (CAD-1574); other kinds stay a label. */
function RefChips({ refs, onOpenIssue }: { refs: ThreadRef[]; onOpenIssue?: (id: string) => void }) {
  const t = useTranslate();
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

/** CAD-1462: past this many sent files the extras sit behind a +N more
 *  disclosure — the same collapse point the draft strip uses. */
const SENT_CHIP_LIMIT = 3;

/** The operator bubble's retained files as the mock's `.sentchips` row:
 *  compact read-only chips — extension tag, name, `size · kind` — that
 *  wrap beside the bubble and collapse past three behind a
 *  keyboard-accessible +N more. Names, sizes and MIMEs only: a chip is
 *  never a link and never carries a token or path; the full name, byte
 *  size and MIME sit on the chip's `title`, so truncation hides nothing
 *  the operator cannot reach. No remove or retry affordance — these
 *  already sent. */
function AttachmentRows({ files }: { files: EntryAttachment[] }) {
  const t = useTranslate();
  const [expanded, setExpanded] = useState(false);
  if (files.length === 0) return null;
  const collapsed = !expanded && files.length > SENT_CHIP_LIMIT;
  const shown = collapsed ? files.slice(0, SENT_CHIP_LIMIT) : files;
  return (
    <div className="sentchips" data-entry-attachments>
      {shown.map((f) => (
        <span
          key={f.id}
          className="sentchip"
          title={`${f.name} · ${f.size} B · ${f.mime}`}
        >
          <span className="sentchip-ext num" aria-hidden>
            {sentExt(f.name)}
          </span>
          <span className="sentchip-name">{f.name}</span>
          <span className="sentchip-meta num">{attachMeta(f.name, f.size, t)}</span>
        </span>
      ))}
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

/** `9s` / `1m 4s` from two entry stamps; null when they do not measure. The
 *  trace never keeps a clock of its own — the dock's working row does. */
function traceElapsed(from: string | null | undefined, to: string | null | undefined): string | null {
  if (!from || !to) return null;
  const start = Date.parse(from);
  const end = Date.parse(to);
  if (!Number.isFinite(start) || !Number.isFinite(end) || end <= start) return null;
  const secs = Math.max(0, Math.round((end - start) / 1000));
  if (secs < 1) return null;
  if (secs < 60) return `${secs}s`;
  return `${Math.floor(secs / 60)}m ${secs % 60}s`;
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

/** Three typing dots: the only streaming affordance. The transport sends
 *  whole entries, never token deltas, so there is no skeleton and no
 *  fabricated answer body to show while a turn works. The row is not its
 *  own live region: it mounts inside the thread's `role="log"` list, which
 *  already announces the addition, so a second `role="status"` here would
 *  announce the same row twice. */
function TypingDots() {
  const t = useTranslate();
  const actor = useAssistantActor();
  return (
    <div className="typing" data-kind="typing">
      <span className="typing-dots" aria-hidden>
        <i />
        <i />
        <i />
      </span>
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
  elapsed,
}: {
  items: ThreadItem[];
  steps: number;
  running: boolean;
  summary: string;
  elapsed: string | null;
}) {
  const t = useTranslate();
  const [open, setOpen] = useState(false);
  const id = useId();
  const title = running ? t("Thinking…") : elapsed ? `${t("Thought for")} ${elapsed}` : t("Reasoning");
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
        {summary !== "" && <span className="turntrace-summary num truncate min-w-0">· {summary}</span>}
        <span className="turntrace-count num shrink-0">
          {steps} {steps === 1 ? t("step") : t("steps")}
        </span>
      </button>
      <div className="steps-body" id={id} inert={!open} aria-hidden={!open}>
        <div className="turntrace-wrap">
          <ol className="turntrace-list">
            {items.map((item) => {
              if (item.type === "tools") {
                return (
                  <li key={item.key} className="turntrace-item">
                    <ToolRows entries={item.entries} />
                  </li>
                );
              }
              if (item.type !== "thinking" && item.type !== "commentary") return null;
              const withheld = item.type === "thinking" && carriesSecret(item.entry.text);
              return (
                <li key={item.key} className="turntrace-item">
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
        </div>
      </div>
    </div>
  );
}

/** The one Copy affordance for an answer, hover/focus-revealed in the turn
 *  head so it never sits on top of the answer text. */
function CopyControl({ text }: { text: string }) {
  const t = useTranslate();
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="lnk turn-copy"
      aria-label={t("Copy answer")}
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
            <Md text={entry.text} onOpen={onOpenIssue} />
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
}: {
  items: ThreadItem[];
  running?: boolean;
  density?: Density;
  /** Only a transcript with no `turn_result` anywhere may treat its last
   *  worker note as an older answer; a new transcript's intermediate note
   *  must stay in the trace. */
  allowLegacy?: boolean;
  onOpenIssue?: (id: string) => void;
  /** Plan cards and the correction report, under the answer. */
  after?: ReactNode;
}) {
  const t = useTranslate();
  const actor = useAssistantActor();
  const traceItems = items.filter(
    (item) => item.type === "thinking" || item.type === "commentary" || item.type === "tools",
  );
  const answer = items.find((item) => item.type === "answer");
  const artifacts = items.filter((item) => item.type === "artifact");
  // A transcript from before `turn_result` has no answer item: its last
  // worker note IS what the operator read as the reply. Only the LAST
  // non-artifact item can be that note — a note followed by tools is a
  // tool-turn step, not a reply, and must stay in the trace.
  const lastNote = [...items].reverse().find((item) => item.type !== "artifact");
  const legacy =
    allowLegacy && !answer && !running && lastNote?.type === "commentary" ? lastNote : null;
  const trace = legacy ? traceItems.filter((item) => item.key !== legacy.key) : traceItems;
  const shown = answer ?? legacy;
  // Streaming is a turn the host says is running AND that has no answer yet:
  // a turn whose answer already landed keeps its finished look.
  const streaming = running && shown === null;
  const failure = shown ? turnFailure(shown.entry, t) : null;
  const hasAnswer = shown !== null && (shown.entry.text.trim() !== "" || failure !== null);
  const noAnswer = !streaming && !hasAnswer;
  const steps = trace.reduce(
    (count, item) => count + (item.type === "tools" ? toolSteps(item.entries).length : 1),
    0,
  );
  const intro = trace.find((item) => item.type === "thinking") ?? trace[0];
  const introEntry =
    intro === undefined ? undefined : intro.type === "tools" ? intro.entries[0] : intro.entry;
  const summary =
    introEntry && !carriesSecret(introEntry.text) ? stepSummary(introEntry.text) : "";
  const elapsed = streaming ? null : traceElapsed(introEntry?.created, shown?.entry.created);
  const headAt = shown?.entry.created ?? lastCreated(items);
  return (
    <div className="turn min-w-0" data-kind="turn" data-turn-state={streaming ? "running" : "complete"}>
      <div className="turn-head">
        <AssistantFace size={16} />
        <span className="turn-name">{displayName(actor.read, actor.scope, t)}</span>
        <span className="turn-time num">{streaming ? t("now") : time(headAt)}</span>
        {shown && shown.entry.text.trim() !== "" && <CopyControl text={shown.entry.text} />}
      </div>
      {trace.length > 0 && (
        <TurnTrace
          items={trace}
          steps={steps}
          running={streaming}
          summary={summary}
          elapsed={elapsed}
        />
      )}
      {shown && <AnswerSurface entry={shown.entry} density={density} onOpenIssue={onOpenIssue} earlier={legacy !== null} />}
      {noAnswer && (
        <div className="noanswer" data-kind="no-answer">
          {t("No answer returned")}
        </div>
      )}
      {streaming && <TypingDots />}
      {artifacts.length > 0 && (
        <div className="turn-artifacts">
          {artifacts.map((item) =>
            item.type === "artifact" ? <ArtifactCard key={item.key} entry={item.entry} /> : null,
          )}
        </div>
      )}
      {after}
    </div>
  );
}
