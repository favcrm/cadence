import { useWriteBlock } from "../auth/WriteGate";
import { memo, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, ApiError } from "../../lib/api";
import type { ResourceState } from "../../lib/cache";
import { resources, threadReader } from "../../lib/resources";
import { streamInto, type SseErrorState } from "../../lib/sse";
import { useQuery, useResource } from "../../lib/useResource";
import type { MasterAppOrigin, MasterCommandResult, Overview, ThreadRef } from "../../lib/types";
import Link from "../../ui/Link";
import Md from "../../ui/Md";
import {
  composerBlock,
  kvRows,
  MASTER,
  masterStatus,
  SLASH_COMMANDS,
  START_COMMAND,
  RESUME_COMMAND,
  turnState,
  type MasterStatus,
  type TurnState,
} from "./master";
import Composer from "./Composer";
import { atTail, jumpLabel, tailTop } from "./dock";
import MasterChips from "./MasterChips";
import NeedsRail from "./NeedsRail";
import { askDraft, readRailCollapsed, writeRailCollapsed, type HomeNeed } from "./needs";
import PlanCard from "./PlanCard";
import Conversation from "../app-shell/chat/Conversation";
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
import { sendToMaster } from "./send";
import SinceCard from "./SinceCard";
import {
  discardPending,
  lastSeq,
  applyEarlier,
  fetchEarlier,
  openStep,
  planAnchors,
  reduceFrame,
  stepSummary,
  threadItems,
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
 * The in-flight turn line (CAD-551): the pulse and elapsed clock while a
 * turn works, the live step cross-fading as it moves, queued/compacting
 * badges, and Stop → `/stop`. `queued`/`submitting` give the operator
 * the honest "behind a turn"/"still sending" states instead of silence.
 */
function appOriginHref(origin: MasterAppOrigin | null): string | null {
  if (!origin || typeof origin !== "object") return null;
  const { install_id, context_id, conversation_id } = origin;
  // Strict, fail-closed ids: malformed or foreign shapes give no link. An
  // empty context is a real destination (a no-context app conversation),
  // so it only omits the `ctx` selector.
  const strict = (id: unknown): id is string => typeof id === "string" && id.length > 0 && id.trim() === id;
  if (!strict(install_id) || !strict(conversation_id)) return null;
  if (context_id !== "" && !strict(context_id)) return null;
  const ctx = context_id === "" ? "" : `ctx=${encodeURIComponent(context_id)}&`;
  return `/app-installations/${encodeURIComponent(install_id)}?${ctx}conversation=${encodeURIComponent(conversation_id)}`;
}

function WorkingRow({
  turn,
  appOrigin,
  step,
  onStop,
}: {
  turn: TurnState;
  appOrigin: MasterAppOrigin | null;
  step: ThreadEntry | null;
  onStop: () => void;
}) {
  const appHref = appOriginHref(appOrigin);
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
      {appHref && (turn.kind === "working" || turn.kind === "queued") && (
        <Link href={appHref} className="lnk text-micro shrink-0">
          Open its conversation <span aria-hidden>↗</span>
        </Link>
      )}
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

function NotStarted({ status, hosted }: { status: MasterStatus; hosted: boolean | null }) {
  const stopped = status.kind === "stopped";
  return (
    <div className="card px-4 py-4 space-y-3" data-empty="master">
      <h2 className="text-cardtitle font-semibold text-ink-100">
        {hosted === null ? "Checking assistant availability…" : hosted ? "Your assistant is unavailable" : stopped ? "The master is stopped" : "Start the master to chat"}
      </h2>
      <p className="text-secondary text-ink-400">
        {hosted === null ? "Checking this board’s session and assistant state." : hosted ? "Your workspace administrator can check the assistant’s setup and restore it. You can send messages once it is available." : "The master is the agent you talk to here. It plans work with you and hands approved tickets to your agents. Start or resume it from a terminal on this machine:"}
      </p>
      {hosted === false && <pre className="num text-secondary text-ink-200 bg-ink-800 rounded px-3 py-2 overflow-x-auto">
        {stopped ? RESUME_COMMAND : START_COMMAND}
      </pre>}
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

const retry = (message: string, text: string, refs?: ThreadRef[], attachments?: { id: string }[]) =>
  sendToMaster(text, message, refs, attachments);
const discard = (message: string) => resources.masterThread.write((s) => discardPending(s, message));

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
      <Conversation
        mode={{ kind: "home" }}
        items={shown}
        readOnly={readOnly}
        liveAfter={liveAfter}
        onOpenIssue={onOpenIssue}
        onRetry={onRetry}
        onDiscard={discard}
        after={(item) =>
          anchors.has(item.key) ? (
            <div className="ml-8">
              <PlanCard epic={anchors.get(item.key)!} readOnly={readOnly} onOpenIssue={onOpenIssue} />
            </div>
          ) : null
        }
      />
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
  hosted = null,
  overview,
  onOpenIssue,
  overviewHref,
  permissionsHref,
  setupNotice = null,
}: {
  readOnly: boolean;
  hosted?: boolean | null;
  overview: ResourceState<Overview>;
  onOpenIssue: (id: string) => void;
  overviewHref: string;
  /** Settings → Master permissions (the standing "Always" rules). */
  permissionsHref: string;
  /** The first-run setup strip (CAD-1312): rendered inside the chat
   *  panel below the Assistant heading so the Needs-you rail beside it
   *  keeps the workspace's top edge. */
  setupNotice?: ReactNode;
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
  const block = composerBlock(writeBlock, status, hosted);
  const items = useMemo(() => threadItems(thread.data), [thread.data]);
  const loaded = thread.data !== null;
  const moreBefore = thread.data?.moreBefore === true;

  const masterRow = agents.data?.agents.find((a) => a.alias === MASTER);
  const submitting = (thread.data?.pending ?? []).some((p) => p.state !== "failed");
  const turn = turnState(master.data, masterRow, submitting);
  const serverTurn = master.data?.turn;
  // `turnState` retains the message id for working turns. For queued turns
  // it drops that id, so recover it only when its server-derived display
  // fields still match the same queued master turn (before its row fallback).
  const displayedTurnMessage =
    turn.kind === "working"
      ? turn.message
      : turn.kind === "queued" &&
          serverTurn?.state === "queued" &&
          turn.summary === serverTurn.summary &&
          turn.since === serverTurn.since
        ? serverTurn.message
        : undefined;
  const turnOrigin =
    master.status === "ok" &&
    !master.inFlight &&
    serverTurn !== undefined &&
    serverTurn !== null &&
    displayedTurnMessage !== undefined &&
    displayedTurnMessage === serverTurn.message &&
    serverTurn.app_origin != null &&
    serverTurn.conversation === serverTurn.app_origin.conversation_id
      ? serverTurn.app_origin
      : null;
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
    (message: string, text: string, refs?: ThreadRef[], attachments?: { id: string }[]) => {
      engage();
      retry(message, text, refs, attachments);
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
  const onAsk = (need: HomeNeed, lead?: string) => {
    const draft = askDraft(need, lead);
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
          permissionsHref={permissionsHref}
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
            <span className="text-accent text-section" aria-hidden>✳</span>
            <h1 className="text-section font-semibold text-ink-100">Assistant</h1>
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

          {setupNotice}

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
                <NotStarted status={status} hosted={hosted} />
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
            <WorkingRow turn={turn} appOrigin={turnOrigin} step={step} onStop={() => runCommand("stop", "")} />
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
