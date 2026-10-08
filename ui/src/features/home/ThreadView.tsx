import { useMemo, useState, type ReactNode } from "react";
import type { ThreadRef } from "../../lib/types";
import Md from "../../ui/Md";
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
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/**
 * A run of tool calls and results (CAD-551): one collapsed row that
 * opens into the paired steps. A refusal reads "refused", not "error";
 * a call still running shows the pulse instead of a result. Opening and
 * closing animate through the grid-rows height trick (styles.css).
 */
function StepsGroup({ entries, density = "full" }: { entries: ThreadEntry[]; density?: Density }) {
  const [open, setOpen] = useState(false);
  const steps = useMemo(() => toolSteps(entries), [entries]);
  const refused = steps.filter((s) => s.refused).length;
  const failed = steps.filter((s) => s.error && !s.refused).length;
  const running = steps.some((s) => !s.done);
  const last = steps[steps.length - 1];
  return (
    <div className={`steps ${density === "compact" ? "ml-7" : "ml-8"} min-w-0`} data-kind="tools" data-open={open || undefined}>
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
          {steps.length} tool {steps.length === 1 ? "step" : "steps"}
        </span>
        {running && <span className="steps-running shrink-0">· running</span>}
        {failed > 0 && (
          <span className="text-fail shrink-0">· {failed === 1 ? "error" : `${failed} errors`}</span>
        )}
        {refused > 0 && (
          <span className="text-warn shrink-0">· {refused === 1 ? "refused" : `${refused} refused`}</span>
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
              {s.refused && <span className="step-tag text-warn shrink-0">refused</span>}
              {s.error && !s.refused && <span className="step-tag text-fail shrink-0">error</span>}
              {s.result && s.call.kind === "tool_call" && (
                <span className="num truncate max-w-[40%] text-ink-500" title={s.result.text}>
                  {stepSummary(s.result.text)}
                </span>
              )}
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}

function Bubble({
  who,
  at,
  children,
  me,
  density = "full",
}: {
  who: string;
  at?: string;
  children: ReactNode;
  me?: boolean;
  density?: Density;
}) {
  const compact = density === "compact";
  return (
    <div className={`flex gap-2.5 min-w-0 ${me ? "flex-row-reverse" : ""}`}>
      <span
        className={`shrink-0 ${compact ? "w-5 h-5" : "w-6 h-6"} rounded-full grid place-items-center text-micro font-semibold ${
          me ? "bg-accent/15 text-accent" : "bg-ink-800 text-ink-300"
        }`}
        aria-hidden
      >
        {me ? "Y" : "M"}
      </span>
      <div className={`min-w-0 ${compact ? "max-w-[92%]" : "max-w-[85%]"} ${me ? "text-right" : ""}`}>
        <div className="text-micro text-ink-500">
          <span className="text-ink-300 font-medium">{who}</span>
          {at ? ` · ${at}` : ""}
        </div>
        {children}
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
}: {
  item: ThreadItem;
  density?: Density;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
  onRetry: (
    message: string,
    text: string,
    refs?: ThreadRef[],
    attachments?: { id: string }[],
  ) => void;
  onDiscard: (message: string) => void;
}) {
  const compact = density === "compact";
  const bodyText = compact ? "text-secondary" : "text-body";
  switch (item.type) {
    case "operator":
      return (
        <Bubble who="You" at={time(item.entry.created)} me density={density}>
          <div className={`inline-block text-left mt-0.5 px-3 py-2 rounded-lg bg-accent/10 ${bodyText} text-ink-100 whitespace-pre-wrap break-words`}>
            {item.entry.text}
          </div>
          <RefChips refs={entryRefs(item.entry.payload)} />
          <AttachmentRows files={entryAttachments(item.entry.payload)} />
        </Bubble>
      );
    case "pending":
      return (
        <Bubble
          who="You"
          at={item.pending.state === "failed" ? "not sent" : item.pending.state === "sent" ? "queued" : "sending…"}
          me
          density={density}
        >
          <div
            className={`inline-block text-left mt-0.5 px-3 py-2 rounded-lg ${bodyText} whitespace-pre-wrap break-words ${
              item.pending.state === "failed" ? "border border-fail/50 text-ink-200" : "bg-accent/5 text-ink-300"
            }`}
            data-pending={item.pending.state}
          >
            {item.pending.text}
          </div>
          <RefChips refs={item.pending.refs ?? []} />
          {item.pending.state === "failed" && (
            <div className="text-micro text-fail mt-1 break-words">
              {item.pending.error}{" "}
              <button
                className="lnk"
                onClick={() =>
                  onRetry(
                    item.pending.message,
                    item.pending.text,
                    item.pending.refs,
                    item.pending.attachments,
                  )
                }
              >
                Retry
              </button>{" "}
              ·{" "}
              <button className="lnk" onClick={() => onDiscard(item.pending.message)}>
                Discard
              </button>
            </div>
          )}
        </Bubble>
      );
    case "commentary":
      return (
        <div
          className={`${compact ? "ml-7" : "ml-8"} text-secondary text-ink-400 italic whitespace-pre-wrap break-words`}
          data-kind="commentary"
        >
          {item.entry.text}
        </div>
      );
    case "tools":
      return <StepsGroup entries={item.entries} density={density} />;
    case "answer":
      return (
        <Bubble who="Master" at={time(item.entry.created)} density={density}>
          <div className={`issue-reader mt-0.5 ${bodyText} text-ink-200 break-words`} data-kind="answer">
            <Md text={item.entry.text} onOpen={onOpenIssue} />
          </div>
        </Bubble>
      );
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
  onOpenIssue: (id: string) => void;
}) {
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
        <span className="shrink-0">{briefing ? "Session briefing" : "Details"}</span>
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
 *  each under the text. */
function RefChips({ refs }: { refs: ThreadRef[] }) {
  if (refs.length === 0) return null;
  return (
    <span className="refsrow" aria-label="cited rows">
      {refs.map((r) => (
        <span key={`${r.kind}:${r.id}`} className="refchip num" title={`${r.kind}:${r.id}`}>
          {r.kind}:{r.id}
        </span>
      ))}
    </span>
  );
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

/** The operator bubble's retained-file rows — names, sizes, MIMEs only:
 *  a chip is never a link and never carries a token or path. */
function AttachmentRows({ files }: { files: EntryAttachment[] }) {
  if (files.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-1 mt-1 justify-end" data-entry-attachments>
      {files.map((f) => (
        <span key={f.id} className="chip text-micro" title={`${f.mime} · ${f.size} B`}>
          {f.name} · {(f.size / 1024).toFixed(f.size > 1024 ? 0 : 1)} KB
        </span>
      ))}
    </div>
  );
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
