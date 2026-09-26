import { useWriteBlock } from "../auth/WriteGate";
import { memo, useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { api, ApiError } from "../../lib/api";
import type { ResourceState } from "../../lib/cache";
import { resources, threadReader } from "../../lib/resources";
import { streamInto, type SseErrorState } from "../../lib/sse";
import { useQuery, useResource } from "../../lib/useResource";
import type { MasterCommandResult, Overview, ThreadRef } from "../../lib/types";
import Button from "../../ui/Button";
import Md from "../../ui/Md";
import {
  composerBlock,
  kvRows,
  MASTER,
  masterStatus,
  parseSlash,
  SLASH_COMMANDS,
  slashMatches,
  START_COMMAND,
  turnState,
  type MasterStatus,
  type SlashCommand,
  type TurnState,
} from "./master";
import { atTail, jumpLabel, tailTop } from "./dock";
import MasterChips from "./MasterChips";
import NeedsRail from "./NeedsRail";
import { askDraft, readRailCollapsed, writeRailCollapsed, type HomeNeed } from "./needs";
import PlanCard from "./PlanCard";
import {
  initialFollow,
  motionBehavior,
  onOperatorScrollUp,
  onPill,
  onScrollSettled,
  onSend,
  onTailChange,
  onViewportScroll,
  type Follow,
} from "./scrollFollow";
import SinceCard from "./SinceCard";
import {
  addPending,
  discardPending,
  lastSeq,
  applyEarlier,
  fetchEarlier,
  newMessageId,
  openStep,
  planAnchors,
  reduceFrame,
  settlePending,
  stepSummary,
  threadItems,
  toolSteps,
  visibleWindow,
  WINDOW,
  type ThreadEntry,
  type ThreadItem,
} from "./thread";

const EXAMPLES = [
  "Plan a small feature for one of my projects and show me the tickets before anyone starts.",
  "What is every agent working on right now, and is anything stuck?",
  "Summarise what changed on the tracker since yesterday.",
];

function time(created: string | null | undefined): string {
  if (!created) return "";
  const d = new Date(created);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** `42s` → `2m 5s` → `1h 3m` — the working row's elapsed read-out. */
function fmtElapsed(secs: number): string {
  if (secs < 60) return `${secs}s`;
  const m = Math.floor(secs / 60);
  if (m < 60) return `${m}m ${secs % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

const reducedMotion = () =>
  typeof matchMedia === "function" && matchMedia("(prefers-reduced-motion: reduce)").matches;

/**
 * A run of tool calls and results (CAD-551): one collapsed row that
 * opens into the paired steps. A refusal reads "refused", not "error";
 * a call still running shows the pulse instead of a result. Opening and
 * closing animate through the grid-rows height trick (styles.css).
 */
function StepsGroup({ entries }: { entries: ThreadEntry[] }) {
  const [open, setOpen] = useState(false);
  const steps = useMemo(() => toolSteps(entries), [entries]);
  const refused = steps.filter((s) => s.refused).length;
  const failed = steps.filter((s) => s.error && !s.refused).length;
  const running = steps.some((s) => !s.done);
  const last = steps[steps.length - 1];
  return (
    <div className="steps ml-8 min-w-0" data-kind="tools" data-open={open || undefined}>
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

function Bubble({ who, at, children, me }: { who: string; at?: string; children: ReactNode; me?: boolean }) {
  return (
    <div className={`flex gap-2.5 min-w-0 ${me ? "flex-row-reverse" : ""}`}>
      <span
        className={`shrink-0 w-6 h-6 rounded-full grid place-items-center text-micro font-semibold ${
          me ? "bg-accent/15 text-accent" : "bg-ink-800 text-ink-300"
        }`}
        aria-hidden
      >
        {me ? "Y" : "M"}
      </span>
      <div className={`min-w-0 max-w-[85%] ${me ? "text-right" : ""}`}>
        <div className="text-micro text-ink-500">
          <span className="text-ink-300 font-medium">{who}</span>
          {at ? ` · ${at}` : ""}
        </div>
        {children}
      </div>
    </div>
  );
}

function Item({
  item,
  onOpenIssue,
  onRetry,
  onDiscard,
}: {
  item: ThreadItem;
  onOpenIssue: (id: string) => void;
  onRetry: (message: string, text: string, refs?: ThreadRef[]) => void;
  onDiscard: (message: string) => void;
}) {
  switch (item.type) {
    case "operator":
      return (
        <Bubble who="You" at={time(item.entry.created)} me>
          <div className="inline-block text-left mt-0.5 px-3 py-2 rounded-lg bg-accent/10 text-body text-ink-100 whitespace-pre-wrap break-words">
            {item.entry.text}
          </div>
          <RefChips refs={entryRefs(item.entry.payload)} />
        </Bubble>
      );
    case "pending":
      return (
        <Bubble
          who="You"
          at={item.pending.state === "failed" ? "not sent" : item.pending.state === "sent" ? "queued" : "sending…"}
          me
        >
          <div
            className={`inline-block text-left mt-0.5 px-3 py-2 rounded-lg text-body whitespace-pre-wrap break-words ${
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
                onClick={() => onRetry(item.pending.message, item.pending.text, item.pending.refs)}
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
        <div className="ml-8 text-secondary text-ink-400 italic whitespace-pre-wrap break-words" data-kind="commentary">
          {item.entry.text}
        </div>
      );
    case "tools":
      return <StepsGroup entries={item.entries} />;
    case "answer":
      return (
        <Bubble who="Master" at={time(item.entry.created)}>
          <div className="issue-reader mt-0.5 text-body text-ink-200 break-words" data-kind="answer">
            <Md text={item.entry.text} onOpen={onOpenIssue} />
          </div>
        </Bubble>
      );
    case "system": {
      const e = item.entry;
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

/**
 * The in-flight turn line (CAD-551): the pulse and elapsed clock while a
 * turn works, the live step cross-fading as it moves, queued/compacting
 * badges, and Stop → `/stop`. `queued`/`submitting` give the operator
 * the honest "behind a turn"/"still sending" states instead of silence.
 */
function WorkingRow({
  turn,
  step,
  onStop,
}: {
  turn: TurnState;
  step: ThreadEntry | null;
  onStop: () => void;
}) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (turn.kind !== "working") return;
    const t = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(t);
  }, [turn.kind]);
  if (turn.kind === "idle") return null;
  const stepText = step ? stepSummary(step.text) : null;
  const elapsed =
    turn.kind === "working" &&
    typeof turn.since === "number" &&
    Number.isFinite(turn.since)
      ? fmtElapsed(Math.max(0, Math.floor(now / 1000 - turn.since)))
      : null;
  return (
    <div className="workrow" role="status" aria-live="polite" data-turn={turn.kind}>
      <span className={`workdot${turn.kind === "working" ? " live" : ""}`} aria-hidden />
      {turn.kind === "working" && (
        <span className="text-ink-200 font-medium shrink-0">Working</span>
      )}
      {elapsed !== null && <span className="num text-ink-500 shrink-0">{elapsed}</span>}
      {turn.kind === "queued" && (
        <span className="text-ink-400 min-w-0">
          Queued{turn.summary ? ` — ${stepSummary(turn.summary)}` : ""}
        </span>
      )}
      {turn.kind === "submitting" && <span className="text-ink-400">Sending…</span>}
      {turn.kind === "working" && stepText && (
        <span key={stepText} className="stepfade num text-ink-500 truncate min-w-0">
          · {stepText}
        </span>
      )}
      {turn.kind === "working" && turn.compacting === true && (
        <span className="chip bg-warn/10 text-warn shrink-0">compacting</span>
      )}
      {turn.kind === "working" && (turn.queued ?? 0) > 0 && (
        <span className="chip bg-ink-800 text-ink-400 shrink-0">{turn.queued} queued</span>
      )}
      <span className="flex-1" />
      {turn.kind === "working" && (
        <button
          type="button"
          className="stopbtn shrink-0"
          onClick={onStop}
          title="Interrupt the running turn (same as /stop)"
        >
          Stop
        </button>
      )}
    </div>
  );
}

/** A `/` command's answer card (CAD-551), kept client-side — a read's
 * result, a mutation's ack (the durable system line also lands via the
 * stream), or the failure the daemon returned. */
interface CmdResult {
  id: number;
  name: string;
  arg?: string;
  running?: boolean;
  ok?: boolean;
  text?: string;
  /** A JSON answer — listed as fields, with raw JSON one toggle away. */
  value?: unknown;
}

/**
 * A JSON command answer (CAD-551 r2): the fields as a compact key/value
 * list — model, effort, session, context, pending first — with the raw
 * pretty JSON one toggle away, never the only read.
 */
function CmdJson({ value }: { value: unknown }) {
  const [raw, setRaw] = useState(false);
  const rows = useMemo(() => kvRows(value), [value]);
  const json = useMemo(() => JSON.stringify(value, null, 2), [value]);
  return (
    <div className="mt-1 min-w-0" data-kind="cmdjson">
      {rows.length > 0 && (
        <div className="flex justify-end -mt-1">
          <button type="button" className="lnk text-micro" onClick={() => setRaw((r) => !r)}>
            {raw ? "fields" : "raw"}
          </button>
        </div>
      )}
      {!raw && rows.length > 0 ? (
        <dl className="kvlist">
          {rows.map(([k, v]) => (
            <div key={k} className="kvrow">
              {/* CAD-569: the wrapped label keeps the full key one
                  hover away when a long wire name folds. */}
              <dt title={k}>{k}</dt>
              <dd>{v}</dd>
            </div>
          ))}
        </dl>
      ) : (
        <pre className="num text-micro overflow-x-auto">{json}</pre>
      )}
    </div>
  );
}

function CmdCard({ cmd, onOpenIssue }: { cmd: CmdResult; onOpenIssue: (id: string) => void }) {
  return (
    <div className="cmdcard card px-3 py-2 ml-8 min-w-0" data-cmd={cmd.name}>
      <div className="flex items-center gap-2 text-micro">
        <span className="num text-accent font-medium">
          /{cmd.name}
          {cmd.arg ? ` ${cmd.arg}` : ""}
        </span>
        {cmd.running ? (
          <span className="steps-running text-ink-500">running…</span>
        ) : cmd.ok === false ? (
          <span className="text-fail">failed</span>
        ) : (
          <span className="text-ink-500">done</span>
        )}
      </div>
      {cmd.value !== undefined ? (
        <CmdJson value={cmd.value} />
      ) : (
        cmd.text && (
          <div className="issue-reader mt-1 text-secondary text-ink-300 break-words min-w-0">
            <Md text={cmd.text} onOpen={onOpenIssue} />
          </div>
        )
      )}
    </div>
  );
}

/** `/help` — the catalog rendered as a GFM table. */
function helpText(): string {
  const rows = SLASH_COMMANDS.filter((c) => c.name !== "help").map(
    (c) => `| \`/${c.name}${c.arg ? ` ${c.arg}` : ""}\` | ${c.blurb} |`,
  );
  return `**Commands**\n\n| command | what it does |\n|---|---|\n${rows.join("\n")}`;
}

/** What a `masterCommand` answer shows on its card: a string result is
 * text; a JSON payload is `value` (listed as fields, raw behind a
 * toggle); null is a bare ack. */
function fmtCommandResult(r: MasterCommandResult): { text?: string; value?: unknown } {
  const v = r.result;
  if (typeof v === "string") return { text: v };
  if (v == null) return { text: r.ok ? "done" : "no result" };
  return { value: v };
}

function NotStarted({ status }: { status: MasterStatus }) {
  const stopped = status.kind === "stopped";
  return (
    <div className="card px-4 py-4 space-y-3" data-empty="master">
      <h2 className="text-cardtitle font-semibold text-ink-100">
        {stopped ? "The master is stopped" : "Start the master to chat"}
      </h2>
      <p className="text-secondary text-ink-400">
        The master is the agent you talk to here. It plans work with you and hands approved tickets to your
        agents. Start it from a terminal on this machine:
      </p>
      <pre className="num text-secondary text-ink-200 bg-ink-800 rounded px-3 py-2 overflow-x-auto">
        {START_COMMAND}
      </pre>
      <div className="grid sm:grid-cols-2 gap-3 text-label">
        <div>
          <div className="slabel mb-1">it can</div>
          <ul className="list-disc pl-4 space-y-0.5 text-ink-300">
            <li>read the tracker, reports and what agents are doing</li>
            <li>propose a plan — tickets with size and acceptance</li>
            <li>dispatch tickets of a plan you approved</li>
            <li>answer workers' questions, and bring you the ones it can't</li>
          </ul>
        </div>
        <div>
          <div className="slabel mb-1">it can't</div>
          <ul className="list-disc pl-4 space-y-0.5 text-ink-300">
            <li>approve its own plans — only you do, here or with `cadence plan approve`</li>
            <li>start work outside an approved plan</li>
            <li>merge, push or reach your git credentials</li>
            <li>change settings or other agents</li>
          </ul>
        </div>
      </div>
    </div>
  );
}

function Examples({ onPick, disabled }: { onPick: (text: string) => void; disabled: boolean }) {
  return (
    <div className="card px-4 py-4" data-empty="thread">
      <h2 className="text-cardtitle font-semibold text-ink-100">Ask the master for work</h2>
      <p className="text-secondary text-ink-400 mt-1">No conversation yet. Try one of these:</p>
      <div className="mt-3 grid gap-2">
        {EXAMPLES.map((ex) => (
          <button
            key={ex}
            disabled={disabled}
            onClick={() => onPick(ex)}
            className="text-left px-3 py-2 rounded border border-ink-700 text-secondary text-ink-200 hover:border-accent/60 hover:text-accent disabled:opacity-50 disabled:hover:border-ink-700 disabled:hover:text-ink-200"
          >
            {ex}
          </button>
        ))}
      </div>
    </div>
  );
}

/** Queue `text` to the master: optimistic entry, reconciled by message id.
 *  `refs` are the needs-me subjects an Ask-master draft cites — they
 *  ride `thread_send`'s `refs` onto the entry's payload (CAD-574). */
function sendToMaster(text: string, message = newMessageId(), refs?: ThreadRef[]): void {
  const body = text.trim();
  if (!body) return;
  const store = resources.masterThread;
  store.write((s) => addPending(s, message, body, Date.now(), refs));
  api
    .threadSend(MASTER, body, message, refs)
    .then(() => {
      store.write((s) => settlePending(s, message, { ok: true }));
      // The send queued a turn — refresh the header's state soon.
      void resources.masterState.refresh();
    })
    .catch((e: ApiError) =>
      store.write((s) => settlePending(s, message, { ok: false, error: e.message ?? String(e) })),
    );
}

const retry = (message: string, text: string, refs?: ThreadRef[]) => sendToMaster(text, message, refs);
const discard = (message: string) => resources.masterThread.write((s) => discardPending(s, message));

/**
 * The composer owns its draft: typing re-renders this form only, never
 * the thread above it. `seed` (an example ask) replaces the draft.
 *
 * CAD-551 slash UX: a draft beginning with `/` is a command, not a chat
 * message — the menu completes verbs while the first token is typed,
 * Tab/Enter completes, Esc dismisses, and a complete `/verb [arg]`
 * submits through `onCommand` (never to the thread).
 *
 * CAD-600: this is the floating dock — pinned to the panel's bottom by
 * its wrapper, the textarea growing with the draft up to ~8 lines and
 * scrolling past that (the CSS max-height), and the focus staying in
 * the box after a send.
 */
function Composer({
  block,
  seed,
  onCommand,
  onSent,
}: {
  block: string | null;
  seed: { text: string; n: number; refs?: ThreadRef[] };
  onCommand: (name: string, arg: string) => void;
  /** Enter or the button actually submitted — chat, slash, or an Ask-master draft. */
  onSent: () => void;
}) {
  const [draft, setDraft] = useState("");
  // CAD-574: an Ask-master seed carries the row's subject — chips shown
  // beside the draft, attached to `thread_send` as `refs` on send. A
  // removed chip removes the ref; a sent draft clears them all.
  const [refs, setRefs] = useState<ThreadRef[]>([]);
  const [hi, setHi] = useState(0);
  // Esc dismisses the menu for THIS verb text — the next keystroke
  // reopens it (or a cleared draft does).
  const [menuOffFor, setMenuOffFor] = useState<string | null>(null);
  const form = useRef<HTMLFormElement>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  useEffect(() => {
    if (seed.n > 0) {
      setDraft(seed.text);
      setRefs(seed.refs ?? []);
      if (!block) box.current?.focus();
    }
  }, [seed]);
  // CAD-600: the textarea grows with its content (auto-height), capped
  // by the CSS max-height — past ~8 lines it scrolls instead. A seeded
  // draft grows the same way, and a cleared one shrinks back.
  useEffect(() => {
    const el = box.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, [draft]);

  const slash = parseSlash(draft);
  // The menu shows while the draft is a verb prefix ("/", "/st") — once
  // a space lands the arg is being typed and the menu stands down.
  const verbDraft = draft.startsWith("/") && !draft.includes(" ") ? draft.slice(1) : null;
  const matches =
    verbDraft !== null && draft !== menuOffFor ? slashMatches(verbDraft.toLowerCase()) : [];
  useEffect(() => setHi(0), [verbDraft]);
  const hiClamped = Math.min(hi, Math.max(0, matches.length - 1));

  const complete = (c: SlashCommand) => {
    setDraft(`/${c.name}${c.arg ? " " : ""}`);
    setMenuOffFor(null);
  };

  const submit = () => {
    const body = draft.trim();
    if (!body || block) return;
    // Pin before the pending row or command card paints, so the
    // follow effect scrolls the working indicator into view.
    onSent();
    const cmd = parseSlash(body);
    if (cmd) onCommand(cmd.name, cmd.arg);
    else sendToMaster(body, newMessageId(), refs);
    setDraft("");
    setRefs([]);
    // The dock keeps the focus: a click on Send must not strand the
    // caret on the button — the next message is one keystroke away.
    box.current?.focus();
  };
  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.nativeEvent.isComposing) return;
    if (matches.length) {
      if (e.key === "ArrowDown") {
        e.preventDefault();
        setHi((h) => Math.min(h + 1, matches.length - 1));
        return;
      }
      if (e.key === "ArrowUp") {
        e.preventDefault();
        setHi((h) => Math.max(h - 1, 0));
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setMenuOffFor(draft);
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        complete(matches[hiClamped]);
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        // An exact verb submits; a prefix completes to the highlighted row.
        if (slash && SLASH_COMMANDS.some((c) => c.name === slash.name)) submit();
        else complete(matches[hiClamped]);
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      submit();
    }
  };
  return (
    <form
      ref={form}
      className="card p-2.5 relative"
      data-composer
      data-blocked={block ? "" : undefined}
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      {block && <div className="text-label text-ink-300 mb-2 font-medium">Read-only conversation</div>}
      {matches.length > 0 && (
        <ul className="slashmenu" role="listbox" aria-label="slash commands">
          {matches.map((c, i) => (
            <li key={c.name} role="option" aria-selected={i === hiClamped}>
              <button
                type="button"
                className={`slashrow${i === hiClamped ? " on" : ""}`}
                onMouseEnter={() => setHi(i)}
                onClick={() => complete(c)}
              >
                <span className="num text-accent shrink-0">/{c.name}</span>
                {c.arg && <span className="num text-ink-500 shrink-0">{c.arg}</span>}
                <span className="flex-1 min-w-0 truncate text-ink-500 text-left">{c.blurb}</span>
                <span
                  className={`chip shrink-0 ${
                    c.kind === "read"
                      ? "bg-ink-800 text-ink-500"
                      : c.kind === "set"
                        ? "bg-info/10 text-info"
                        : "bg-warn/10 text-warn"
                  }`}
                >
                  {c.kind}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {refs.length > 0 && (
        <div className="refsrow" aria-label="cited rows" data-composer-refs>
          <span className="text-micro text-ink-500">re:</span>
          {refs.map((r) => (
            <button
              key={`${r.kind}:${r.id}`}
              type="button"
              className="refchip num"
              title={`remove the ${r.kind}:${r.id} reference`}
              onClick={() => setRefs((rs) => rs.filter((x) => x !== r))}
            >
              {r.kind}:{r.id} <span aria-hidden>×</span>
            </button>
          ))}
        </div>
      )}
      <textarea
        ref={box}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={onKey}
        rows={2}
        disabled={!!block}
        placeholder={block ? "Read-only · Sending is unavailable" : "Ask Master about your team or plan the next job…"}
        aria-label="message to the master"
        aria-expanded={matches.length > 0 || undefined}
        className="w-full resize-none bg-transparent text-body text-ink-100 placeholder:text-ink-500 outline-none disabled:opacity-50 min-h-[2.75rem] max-h-40 overflow-y-auto"
      />
      <div className="flex items-center gap-2 mt-1.5">
        <p className="text-micro text-ink-500 min-w-0 flex-1 break-words" data-composer-block={block ? "" : undefined}>
          {block ?? (slash ? "Enter runs the command · Tab completes" : "Enter sends · Shift+Enter for a new line")}
        </p>
        <Button
          type="submit"
          variant="primary"
          size="sm"
          className="sendbtn shrink-0"
          disabled={!!block || !draft.trim()}
        >
          {slash ? "Run" : "Send"}
        </Button>
      </div>
    </form>
  );
}

/**
 * The rendered thread: a bounded window of the newest items (a long
 * thread renders at most `limit`), with "load earlier" above it. Memoized
 * — it re-renders when the thread or the window changes, not on
 * unrelated Home state. Items arriving after `liveAfter` (the seq held
 * at mount) carry the enter animation; history paged in above it does
 * not fly in.
 */
const ThreadList = memo(function ThreadList({
  items,
  limit,
  moreBefore,
  loadingEarlier,
  onEarlier,
  readOnly,
  onOpenIssue,
  liveAfter,
  onRetry,
}: {
  items: ThreadItem[];
  limit: number;
  moreBefore: boolean;
  loadingEarlier: boolean;
  onEarlier: () => void;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
  liveAfter: number;
  onRetry: (message: string, text: string, refs?: ThreadRef[]) => void;
}) {
  const { shown, hidden } = useMemo(() => visibleWindow(items, limit), [items, limit]);
  const anchors = useMemo(() => planAnchors(items), [items]);
  if (items.length === 0) return null;
  return (
    <>
      {(hidden > 0 || moreBefore) && (
        <div className="flex justify-center">
          <button className="lnk text-label disabled:opacity-50" disabled={loadingEarlier} onClick={onEarlier}>
            {loadingEarlier ? "Loading…" : "Load earlier messages"}
          </button>
        </div>
      )}
      <ol className="space-y-3 min-w-0" aria-label="messages" data-rendered={shown.length}>
        {shown.map((item) => {
          // Live items (or the operator's pending ones) enter with the
          // rise animation; anything at or below the mount seq is history.
          const live = item.type === "pending" || Number(item.key.slice(1)) > liveAfter;
          return (
            <li key={item.key} className={`min-w-0 space-y-2${live ? " msg-in" : ""}`}>
              <Item item={item} onOpenIssue={onOpenIssue} onRetry={onRetry} onDiscard={discard} />
              {anchors.has(item.key) && (
                <div className="ml-8">
                  <PlanCard epic={anchors.get(item.key)!} readOnly={readOnly} onOpenIssue={onOpenIssue} />
                </div>
              )}
            </li>
          );
        })}
      </ol>
    </>
  );
});

/**
 * Home (CAD-328): the master's thread with a composer, the "since you
 * left" card above it, and the Needs-you rail beside it. CAD-551 adds
 * the header session chips, the working/queued turn row, `/` commands,
 * and the smart-scroll pill. CAD-574 detaches the rail — its own
 * scroll, collapsible, a slide-over drawer under ~1100px — and turns
 * its "copy command" rows into Ask-master prefills with `refs`, plus
 * model/effort dropdowns on the header chips. CAD-600 makes the chat a
 * full-height panel — the thread scrolls inside it, the composer is a
 * floating dock at its bottom, and the empty/not-started states sit
 * centred in it with the dock disabled.
 */
export default function Home({
  readOnly,
  overview,
  onOpenIssue,
  overviewHref,
}: {
  readOnly: boolean;
  overview: ResourceState<Overview>;
  onOpenIssue: (id: string) => void;
  overviewHref: string;
}) {
  const thread = useQuery(resources.masterThread);
  const agents = useResource(resources.agents);
  const master = useQuery(resources.masterState);
  const [seed, setSeed] = useState<{ text: string; n: number; refs?: ThreadRef[] }>({
    text: "",
    n: 0,
  });
  // CAD-574: the rail's collapse is a layout concern — the grid column
  // narrows with it, so the state lives here and the rail renders it.
  const [railCollapsed, setRailCollapsed] = useState(readRailCollapsed);
  const [link, setLink] = useState<SseErrorState | "live" | null>(null);
  const [limit, setLimit] = useState(WINDOW);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  const [cmds, setCmds] = useState<CmdResult[]>([]);
  const [unseen, setUnseen] = useState(0);
  const [glideTick, setGlideTick] = useState(0);
  const cmdSeq = useRef(0);
  const follow = useRef<Follow>(initialFollow());
  const prevItems = useRef(0);
  const liveAfter = useRef<number | null>(null);
  // CAD-600: the panel scrolls, not the page — its own scroller and the
  // dock's measured height drive the follow, the pill and the padding.
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const dockRef = useRef<HTMLDivElement | null>(null);
  /** Older history just paged in above: not an arrival, never a pill. */
  const earlierLanded = useRef(false);

  const missing = thread.data?.missing === true;
  const status = masterStatus(agents.data, missing);
  const writeBlock = useWriteBlock(readOnly);
  const block = composerBlock(writeBlock, status);
  const items = useMemo(() => threadItems(thread.data), [thread.data]);
  const loaded = thread.data !== null;
  const moreBefore = thread.data?.moreBefore === true;

  const masterRow = agents.data?.agents.find((a) => a.alias === MASTER);
  const submitting = (thread.data?.pending ?? []).some((p) => p.state !== "failed");
  const turn = turnState(master.data, masterRow, submitting);
  const step = useMemo(() => openStep(thread.data?.entries ?? []), [thread.data]);

  // Live entries: the store opened on the newest page, so the stream
  // resumes after its last seq — never a replay of the history. On a
  // reconnect `streamInto` resumes with `?after=<last id>` and the
  // reducer dedupes by seq: no gaps, no repeats.
  useEffect(() => {
    if (!loaded || missing) return;
    const sub = streamInto(resources.masterThread, reduceFrame, {
      url: `/api/threads/${MASTER}/stream`,
      events: ["entry"],
      lastEventId: String(lastSeq(resources.masterThread.get().data) ?? 0),
      onError: (state) => {
        setLink(state);
        // The alias went away (a 4xx for good): re-read to show why.
        if (state === "stopped") void resources.masterThread.refresh();
      },
    });
    setLink("live");
    return () => sub.close();
  }, [loaded, missing]);

  // The seq watermark for the enter animation: items at or below the
  // mount tail are history — they render still; only what lands after
  // rises in.
  useEffect(() => {
    if (liveAfter.current === null && loaded) {
      liveAfter.current = lastSeq(thread.data) ?? 0;
    }
  }, [loaded, thread.data]);

  // Keep master_state honest: it follows the agents resource's
  // invalidations (turn boundaries land there), and polls gently while a
  // turn works — context use drifts inside a turn without touching the
  // agents row. A daemon that answered 501 (no master_state) stops the
  // follow — send/command refreshes still retry it explicitly.
  useEffect(() => {
    if (master.status !== "failed") void resources.masterState.refresh();
  }, [agents.data, master.status]);
  useEffect(() => {
    if (turn.kind !== "working") return;
    const t = setInterval(() => void resources.masterState.refresh(), 12_000);
    return () => clearInterval(t);
  }, [turn.kind]);
  // Turn boundaries land on the thread stream before the agents row
  // moves — a `turn_result` or a queued `message` tail revalidates the
  // state immediately so the working row clears with the reply, not a
  // beat after it.
  const tailSeq = thread.data?.entries.at(-1)?.seq ?? 0;
  useEffect(() => {
    const tail = thread.data?.entries.at(-1);
    if (tail && (tail.kind === "turn_result" || tail.kind === "message")) {
      void resources.masterState.refresh();
    }
  }, [tailSeq]);

  // Smart scroll (CAD-551, panel-scoped CAD-600) plus send-pin
  // (CAD-610). Follow the tail while the operator is already there.
  // A send (Enter, the button, a slash command, an Ask-master draft,
  // a retry) pins even from history so the working indicator stays in
  // view. Scrolling up during the turn drops the pin and the next row
  // counts onto the pill.
  const applyFollow = (next: Follow) => {
    const prevUnseen = follow.current.unseen;
    follow.current = next;
    if (next.unseen !== prevUnseen) setUnseen(next.unseen);
  };
  const scrollPanel = useCallback((kind: "glide" | "jump") => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTo({
      top: tailTop(el.scrollHeight, el.clientHeight),
      behavior: motionBehavior(reducedMotion(), kind),
    });
  }, []);
  // A send pins before the pending row paints; the glide runs after it.
  const engage = useCallback(() => {
    const next = onSend(follow.current);
    follow.current = next.state;
    setUnseen(0);
    setGlideTick((n) => n + 1);
  }, []);
  const onRetrySend = useCallback(
    (message: string, text: string, refs?: ThreadRef[]) => {
      engage();
      retry(message, text, refs);
    },
    [engage],
  );
  const onPanelScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const near = atTail(el.scrollTop, el.scrollHeight, el.clientHeight);
    applyFollow(onViewportScroll(follow.current, near));
  }, []);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const nearNow = () => atTail(el.scrollTop, el.scrollHeight, el.clientHeight);
    const onScrollEnd = () => {
      const settled = onScrollSettled(follow.current, nearNow());
      applyFollow(settled.state);
      if (settled.scroll === "jump") scrollPanel("jump");
    };
    // Only an in-flight send glide is cancelled here. Once it has
    // settled, the position check is what stops the pin.
    const leave = () => {
      if (!follow.current.programmatic) return;
      const next = onOperatorScrollUp(follow.current);
      if (next === follow.current) return;
      applyFollow(next);
      el.scrollTo({ top: el.scrollTop, behavior: "auto" });
    };
    const onWheel = (e: WheelEvent) => {
      if (e.deltaY < 0) leave();
    };
    let touchY = 0;
    const onTouchStart = (e: TouchEvent) => {
      touchY = e.touches[0]?.clientY ?? 0;
    };
    const onTouchMove = (e: TouchEvent) => {
      const y = e.touches[0]?.clientY ?? touchY;
      if (y > touchY + 12) leave();
      touchY = y;
    };
    const onKey = (e: globalThis.KeyboardEvent) => {
      if (e.key !== "ArrowUp" && e.key !== "PageUp" && e.key !== "Home") return;
      const target = e.target;
      if (target instanceof Element && target.closest("textarea, input, [contenteditable='true']")) return;
      leave();
    };
    el.addEventListener("scrollend", onScrollEnd);
    el.addEventListener("wheel", onWheel, { passive: true });
    el.addEventListener("touchstart", onTouchStart, { passive: true });
    el.addEventListener("touchmove", onTouchMove, { passive: true });
    addEventListener("keydown", onKey);
    return () => {
      el.removeEventListener("scrollend", onScrollEnd);
      el.removeEventListener("wheel", onWheel);
      el.removeEventListener("touchstart", onTouchStart);
      el.removeEventListener("touchmove", onTouchMove);
      removeEventListener("keydown", onKey);
    };
  }, [scrollPanel]);

  // The dock's measured height is the thread's bottom padding and the
  // pill's floor (`--dock-h`, styles.css); while pinned, a growing dock
  // (a longer draft, a wrapped hint) keeps the tail in view.
  useEffect(() => {
    const dock = dockRef.current;
    if (!dock || typeof ResizeObserver === "undefined") return;
    const apply = () => {
      document.documentElement.style.setProperty("--dock-h", `${dock.offsetHeight}px`);
      if (follow.current.pinned) scrollPanel("jump");
    };
    const ro = new ResizeObserver(apply);
    ro.observe(dock);
    apply();
    return () => {
      ro.disconnect();
      document.documentElement.style.removeProperty("--dock-h");
    };
  }, [scrollPanel]);

  const lastKey = items.length ? items[items.length - 1].key : "";
  useEffect(() => {
    const delta = items.length - prevItems.current;
    prevItems.current = items.length;
    if (earlierLanded.current) {
      earlierLanded.current = false;
      return;
    }
    if (!lastKey) return;
    // A send glide owns the frame it starts in; later rows jump.
    const next = onTailChange(follow.current, delta);
    applyFollow(next.state);
    if (next.scroll === "jump") scrollPanel("jump");
  }, [items.length, lastKey, scrollPanel]);

  // After the pending row, command card, or working indicator paints.
  // Already sitting on the tail skips the glide and drops the lock, so
  // later steps still jump — a glide that never moves would otherwise
  // hold `programmatic` and swallow those follows.
  useEffect(() => {
    if (glideTick === 0) return;
    const el = scrollRef.current;
    if (!el) return;
    const top = tailTop(el.scrollHeight, el.clientHeight);
    if (Math.abs(el.scrollTop - top) <= 1) {
      follow.current = { ...follow.current, pinned: true, programmatic: false };
      return;
    }
    scrollPanel("glide");
  }, [glideTick, scrollPanel]);

  // A slash-command card grows when its answer lands. Follow it while
  // pinned; the send frame itself is owned by the glide above.
  const cmdSig = cmds.map((c) => `${c.id}:${c.running ? 1 : 0}:${c.ok === undefined ? "" : c.ok ? 1 : 0}`).join("|");
  useEffect(() => {
    if (!cmdSig) return;
    const next = onTailChange(follow.current, 0);
    applyFollow(next.state);
    if (next.scroll === "jump") scrollPanel("jump");
  }, [cmdSig, scrollPanel]);

  const jumpToLatest = () => {
    const next = onPill(follow.current);
    follow.current = next.state;
    setUnseen(0);
    scrollPanel(next.scroll);
  };


  /** One `/` verb → the board's command route; its answer is a card. */
  const runCommand = useCallback(
    (name: string, arg: string) => {
      const id = ++cmdSeq.current;
      if (name === "help") {
        setCmds((c) => [...c, { id, name, ok: true, text: helpText() }]);
        return;
      }
      const known = SLASH_COMMANDS.find((c) => c.name === name);
      if (!known) {
        setCmds((c) => [
          ...c,
          { id, name, ok: false, text: `Unknown command — \`/help\` lists what the master accepts.` },
        ]);
        return;
      }
      setCmds((c) => [...c, { id, name, arg: arg || undefined, running: true }]);
      api
        .masterCommand(name, arg || undefined)
        .then((r) => {
          const { text, value } = fmtCommandResult(r);
          setCmds((c) =>
            c.map((x) => (x.id === id ? { ...x, running: false, ok: r.ok, text, value } : x)),
          );
          // Mutations move the session — refresh the chips/state, and the
          // thread after `/new` (the session restart lands entries too).
          void resources.masterState.refresh();
          if (known.kind === "act" || known.kind === "set") void resources.masterThread.refresh();
        })
        .catch((e: ApiError) =>
          setCmds((c) =>
            c.map((x) =>
              x.id === id ? { ...x, running: false, ok: false, text: e.message ?? String(e) } : x,
            ),
          ),
        );
    },
    [],
  );

  const onEarlier = useCallback(() => {
    const data = resources.masterThread.get().data;
    if (!data) return;
    const hidden = Math.max(0, threadItems(data).length - limit);
    if (hidden > 0) {
      setLimit((l) => l + WINDOW);
      return;
    }
    const page = fetchEarlier(threadReader(MASTER), data);
    if (!page) {
      resources.masterThread.write((cur) => ({ ...(cur ?? data), moreBefore: false }));
      return;
    }
    setLoadingEarlier(true);
    page
      .then((older) => {
        earlierLanded.current = true;
        resources.masterThread.write((cur) => applyEarlier(cur, older));
        setLimit((l) => l + WINDOW);
      })
      .catch(() => undefined)
      .finally(() => setLoadingEarlier(false));
  }, [limit]);

  const showNotStarted = status.kind === "absent" || status.kind === "stopped";
  const empty = loaded && !missing && items.length === 0;

  /** Ask master: a prefilled draft with the row's subject as refs —
   *  never sends; the operator reviews it in the composer. */
  const onAsk = (need: HomeNeed) => {
    const draft = askDraft(need);
    setSeed((s) => ({ text: draft.text, refs: draft.refs, n: s.n + 1 }));
  };
  const toggleRail = () => {
    setRailCollapsed((c) => {
      writeRailCollapsed(!c);
      return !c;
    });
  };

  return (
    <main
      className={`home-workspace px-4 lg:px-8 pt-5 pb-3 w-full min-w-0 grid gap-5 grid-rows-[minmax(0,1fr)] flex-1 min-h-0 ${
        railCollapsed
          ? "rail:grid-cols-[minmax(0,1fr)_3rem]"
          : "rail:grid-cols-[minmax(0,1fr)_minmax(0,24rem)]"
      }`}
    >
      {/* The rail is its own column (CAD-574), bounded by the panel's
          height (CAD-600): its `rail-scroll` scrolls independently of
          the thread, and the dock never moves. */}
      <aside className="absolute rail:static min-w-0 rail:order-2 rail:h-full rail:min-h-0">
        <NeedsRail
          overview={overview}
          readOnly={readOnly}
          onOpenIssue={onOpenIssue}
          overviewHref={overviewHref}
          onAsk={onAsk}
          onAskAgent={(agent, update) => setSeed((s) => ({
            text: `Check in with ${agent.alias}${agent.on.length ? ` on ${agent.on.join(", ")}` : ""}. ${update ? `Their latest update: ${update.label}${update.text ? ` — ${update.text.slice(0, 1200)}` : ""}. ` : ""}What is the current progress, and does anything need my input?`,
            refs: [], n: s.n + 1,
          }))}
          collapsed={railCollapsed}
          onToggleCollapse={toggleRail}
        />
      </aside>

      <section className="min-w-0 rail:order-1 flex flex-col min-h-0 gap-4" aria-label="master thread">
        <SinceCard onOpenIssue={onOpenIssue} />

        {/* CAD-600: the conversation panel — the master's header, the
            thread scrolling inside it, and the composer docked to its
            bottom. It fills the height below the board header; the page
            itself never scrolls for the chat. */}
        <div className="relative flex-1 min-h-0 flex flex-col" data-chat-panel>
          <div className="master-heading flex items-center gap-2 flex-wrap pb-2.5 border-b border-ink-700/70">
            <h1 className="text-section font-semibold text-ink-100">Master</h1>
            <span
              className={`chip ${
                status.kind === "running"
                  ? "bg-ok/15 text-ok"
                  : status.kind === "unknown"
                    ? "bg-ink-800 text-ink-400"
                    : "bg-warn/10 text-warn"
              }`}
            >
              {status.kind === "running" ? status.state : status.kind === "absent" ? "not started" : status.kind}
            </span>
            <MasterChips master={master.data} row={masterRow} readOnly={readOnly} />
            {link && link !== "live" && status.kind === "running" && (
              <span className="text-micro text-ink-500">{link === "stopped" ? "stream stopped" : "reconnecting…"}</span>
            )}
          </div>

          <p className="text-label text-ink-500 pt-2 pb-1">Plan work, check in with your agents, and discuss what comes next. <span className="text-ink-400">All projects</span></p>

          {thread.status === "failed" && (
            <div className="card px-3.5 py-3 mt-3 text-label text-fail break-words" role="alert">
              The thread could not be read — {thread.error}{" "}
              <button className="lnk" onClick={() => void resources.masterThread.refresh()}>
                Retry
              </button>
            </div>
          )}
          {!loaded && thread.status !== "failed" && (
            <p className="text-label text-ink-500 pt-3">Reading the thread…</p>
          )}

          <div
            className="chat-scroll min-h-0 flex-1 overflow-y-auto pt-3"
            ref={scrollRef}
            onScroll={onPanelScroll}
            data-chat-scroll
          >
            {showNotStarted && items.length === 0 && (
              <div className="chat-empty">
                <NotStarted status={status} />
              </div>
            )}
            {empty && !showNotStarted && (
              <div className="chat-empty">
                <Examples onPick={(t) => setSeed((s) => ({ text: t, n: s.n + 1 }))} disabled={!!block} />
              </div>
            )}

            <ThreadList
              items={items}
              limit={limit}
              moreBefore={moreBefore}
              loadingEarlier={loadingEarlier}
              onEarlier={onEarlier}
              readOnly={readOnly}
              onOpenIssue={onOpenIssue}
              liveAfter={liveAfter.current ?? 0}
              onRetry={onRetrySend}
            />
            {cmds.map((c) => (
              <CmdCard key={c.id} cmd={c} onOpenIssue={onOpenIssue} />
            ))}
            <WorkingRow turn={turn} step={step} onStop={() => runCommand("stop", "")} />
          </div>

          {unseen > 0 && (
            <button type="button" className="newpill num" onClick={jumpToLatest} data-jump-to-latest>
              ↓ {jumpLabel(unseen)}
            </button>
          )}

          <div className="chatdock" ref={dockRef} data-chat-dock>
            <Composer block={block} seed={seed} onCommand={runCommand} onSent={engage} />
          </div>
        </div>
      </section>
    </main>
  );
}
