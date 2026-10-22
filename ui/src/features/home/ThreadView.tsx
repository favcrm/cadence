import { useMemo, useState, type ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import type { ThreadRef } from "../../lib/types";
import Md from "../../ui/Md";
import { attachMeta } from "../app-shell/chat/attach";
import { askLabel } from "../canvas/askRefs";
import ArtifactCard from "../canvas/ArtifactCard";
import { ThreadPermission } from "./PermissionCard";
import { stepSummary, toolSteps, type ThreadEntry, type ThreadItem } from "./thread";

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

/** A successful `artifact put`: its card already says what the result's raw
 *  JSON would (CAD-1728), so the step row keeps the call and drops the result. */
const isArtifactPut = (text: string) => /\bartifact put\b/.test(text);

/**
 * A run of tool calls and results (CAD-551): one collapsed row that
 * opens into the paired steps. A refusal reads "refused", not "error";
 * a call still running shows the pulse instead of a result. Opening and
 * closing animate through the grid-rows height trick (styles.css).
 */
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
                  {!s.error && isArtifactPut(s.call.text) ? t("in the card") : stepSummary(s.result.text)}
                </span>
              )}
            </li>
          ))}
        </ul>
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
    case "answer": {
      // A failed turn carries its reason in payload.error and no text: say
      // why, instead of an empty bubble. No Retry here — the turn ran.
      const failure = turnFailure(item.entry, t);
      return (
        <Bubble who={t("Assistant")} at={time(item.entry.created)} live={activity}>
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
