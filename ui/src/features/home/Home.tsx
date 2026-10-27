import { useWriteBlock } from "../auth/WriteGate";
import { memo, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { api, ApiError } from "../../lib/api";
import type { ResourceState } from "../../lib/cache";
import { resources, threadReader } from "../../lib/resources";
import type { SseErrorState } from "../../lib/sse";
import { useQuery, useResource } from "../../lib/useResource";
import { useTranslate } from "../../lib/locale";
import type { Agent, MasterAppOrigin, MasterCommandResult, MasterState, Overview, ThreadRef } from "../../lib/types";
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
import { useCanvasFeed } from "../canvas/feed";
import Composer from "./Composer";
import {
  AssistantAboutButton,
  AssistantActorProvider,
  AssistantFace,
  useAssistantActor,
  useConversationActor,
} from "../assistant/AssistantIdentity";
import { displayName, readConversationIdentity, scopeDescriptor } from "../assistant/identity";
import type { AttachDestination } from "../app-shell/chat/attach";
import { atTail, jumpLabel, repinOnResize, showJumpPill, tailTop } from "./dock";
import MasterChips from "./MasterChips";
import NeedsRail from "./NeedsRail";
import { askDraft, readRailCollapsed, writeRailCollapsed, type HomeNeed } from "./needs";
import type { HomeDecisionOpen } from "../../lib/workItems";
import PlanCard from "./PlanCard";
import Conversation from "../app-shell/chat/Conversation";
import { TurnBar } from "../app-shell/chat/TurnBar";
import { usePendingPermission } from "../app-shell/chat/usePendingApproval";
import { noteHomeSeen } from "../app-shell/chat/homeUnread";
import { earlierStep, useThreadStream } from "../app-shell/chat/conversationThread";
import { turnBarState } from "../app-shell/chat/turnBarState";
import { useConversationDestination } from "../app-shell/chat/conversationNavigation";
import MemberChat from "./MemberChat";
import { memberScope } from "./memberThread";
import { CloudContentNotice, StreamBanner, ThreadTurn, time } from "./ThreadView";
import {
  initialFollow,
  motionBehavior,
  onOperatorScrollUp,
  onPill,
  onScrollSettled,
  onSend,
  onTailChange,
  onViewportScroll,
  reducedMotion,
  type Follow,
} from "./scrollFollow";
import { sendToMaster } from "./send";
import { agentOn } from "../../lib/agentOn";
import SinceCard from "./SinceCard";
import {
  discardPending,
  lastSeq,
  newMessageId,
  openStep,
  planAnchors,
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

/**
 * The in-flight turn line (CAD-551): the pulse and elapsed clock while a
 * turn works, the live step cross-fading as it moves, queued/compacting
 * badges, and Stop → `/stop`. `queued`/`submitting` give the operator
 * the honest "behind a turn"/"still sending" states instead of silence.
 */
function appOriginHref(
  origin: MasterAppOrigin | null,
  resolveAppPath: (installId: string) => string | null,
): string | null {
  if (!origin || typeof origin !== "object") return null;
  const { install_id, context_id, conversation_id } = origin;
  // Strict, fail-closed ids: malformed or foreign shapes give no link. An
  // empty context is a real destination (a no-context app conversation),
  // so it only omits the `ctx` selector.
  const strict = (id: unknown): id is string => typeof id === "string" && id.length > 0 && id.trim() === id;
  if (!strict(install_id) || !strict(conversation_id)) return null;
  if (context_id !== "" && !strict(context_id)) return null;
  // CAD-1438: the canonical `/apps/<key>` route, resolved against the
  // authorized install list. An ambiguous, removed or unknown install has
  // no link — never another key's route. `install` pins the immutable id
  // so a reinstalled/renamed app at the same key can never open this
  // context and conversation under the replacement.
  const appPath = resolveAppPath(install_id);
  if (appPath === null) return null;
  const ctx = context_id === "" ? "" : `ctx=${encodeURIComponent(context_id)}&`;
  return `${appPath}?install=${encodeURIComponent(install_id)}&${ctx}conversation=${encodeURIComponent(conversation_id)}`;
}

/** CAD-1462: the subtle live state on the running turn's own name row
 *  (the approved mock's `.who .live`): pulse dot, "Working" and the
 *  elapsed clock right-aligned beside the row's real name and
 *  timestamp. The row itself supplies who/when; Stop and the truthful
 *  queued/submitting/compacting information stay in the dock's status
 *  line, outside the message rows. */
function AssistantActivity({ since }: { since: number | null }) {
  const t = useTranslate();
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  const elapsed = since !== null && Number.isFinite(since) ? fmtElapsed(Math.max(0, Math.floor(now / 1000 - since))) : null;
  return (
    <span className="flex gap-1.5 items-center" data-assistant-activity>
      <span className="workdot live" aria-hidden />
      <span>{t("Working")}</span>
      {/* The 1s-ticking counter is decorative — the dock `.workrow`
         role=status already announces the truthful state, so this
         stays silent (CAD-1419: no elapsed-second SR chatter). */}
      {elapsed !== null && <span className="text-ink-500" aria-hidden>{elapsed}</span>}
    </span>
  );
}

/** A timestamped Assistant name row — the mock's `.who` — for in-flight
 *  content that has no bubble of its own (tool steps, commentary), so
 *  the running turn still shows its name, timestamp and live state on
 *  one header. `at` is the anchored entry's own timestamp. */
function AssistantNameRow({ at, live }: { at?: string; live: ReactNode }) {
  const t = useTranslate();
  const actor = useAssistantActor();
  return (
    <div className="flex gap-2 items-center text-micro text-ink-400" data-kind="assistant-name">
      <span className="text-ink-100 font-medium">{displayName(actor.read, actor.scope, t)}</span>
      {at ? <span>{at}</span> : null}
      {/* The same `.chat-live` wrapper Bubble uses: right-aligned accent. */}
      <span className="chat-live num">{live}</span>
    </div>
  );
}

/** The item the running turn's name-row activity belongs to: the last
 *  thread item holding a stored entry stamped with the turn's own
 *  `message` id. Stored entries are per-turn, so commentary, tool
 *  groups and the answering row of THIS turn carry it; a newer queued
 *  or failed pending message — or another conversation's turn —
 *  carries a different id and can never steal the indicator. A pending
 *  row holds no stored entry yet, so it never matches; an unidentified
 *  turn anchors nothing here rather than guessing a row. */
export function activityAnchor(items: ThreadItem[], message: string | undefined | null): string | null {
  if (typeof message !== "string" || message === "") return null;
  let found: string | null = null;
  let answerKey: string | null = null;
  let cardKey: string | null = null;
  for (const item of items) {
    // An artifact card sits after the run that put it (or between two runs'
    // cards): the header never rides on one while the run itself is there.
    if (item.type === "artifact") {
      if (item.entry.message === message) cardKey = item.key;
      continue;
    }
    const entries =
      item.type === "tools"
        ? item.entries
        : item.type === "pending"
          ? []
          : [item.entry];
    if (entries.some((e) => e.message === message)) {
      found = item.key;
      if (item.type === "answer") answerKey = item.key;
    }
  }
  // An answer bubble carries the indicator on its own timestamped name
  // row — prefer it over in-progress steps/commentary of the same turn.
  return answerKey ?? found ?? cardKey;
}

/** CAD-1881: on a narrow screen the model, effort and About affordances
 *  move behind one overflow sheet, so the header is a single 56px row
 *  instead of a four-line stack. Desktop keeps the chips inline. */
function HeaderMenu({
  master,
  row,
  readOnly,
  scope,
}: {
  master: MasterState | null | undefined;
  row: Agent | undefined;
  readOnly: boolean;
  /** The header's scope line, moved here on a narrow screen. */
  scope: string;
}) {
  const t = useTranslate();
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement | null>(null);
  const panel = useRef<HTMLDivElement | null>(null);
  // A modal sheet: focus enters it, Tab stays inside, Escape closes it, and
  // the trigger gets the focus back — the About dialog's mechanics.
  useEffect(() => {
    if (!open) return;
    const root = panel.current;
    const focusables = () =>
      Array.from(
        root?.querySelectorAll<HTMLElement>(
          'button:not([disabled]), [href], input:not([disabled]), select, textarea, [tabindex]:not([tabindex="-1"])',
        ) ?? [],
      );
    focusables()[0]?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setOpen(false);
        return;
      }
      if (e.key !== "Tab") return;
      const items = focusables();
      if (items.length === 0) return;
      const first = items[0];
      const last = items[items.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("keydown", onKey);
      trigger.current?.focus();
    };
  }, [open]);
  return (
    <span className="assistant-menu">
      <button
        ref={trigger}
        type="button"
        className="assistant-about-trigger assistant-menu-trigger"
        aria-label={t("Assistant settings")}
        title={t("Assistant settings")}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen(true)}
      >
        <span aria-hidden>⋯</span>
      </button>
      {open && (
        <>
          <button
            type="button"
            aria-hidden
            tabIndex={-1}
            className="assistant-menu-scrim"
            onClick={() => setOpen(false)}
          />
          <div ref={panel} className="assistant-menu-panel" role="dialog" aria-modal="true" aria-label={t("Assistant settings")}>
            <div className="assistant-menu-head">
              <span className="text-label font-medium text-ink-200">{t("Assistant settings")}</span>
              <button type="button" className="lnk text-label" onClick={() => setOpen(false)}>
                {t("Close")}
              </button>
            </div>
            <p className="assistant-scope" data-menu-scope>{scope}</p>
            <MasterChips master={master} row={row} readOnly={readOnly} />
            <AssistantAboutButton className="assistant-menu-about" />
          </div>
        </>
      )}
    </span>
  );
}

/** The dock's one-line turn status (CAD-551, placed in `chatdock` by
 *  CAD-1462): the truthful queued/submitting/compacting/queued-count
 *  state, the live step while the turn works, the app-scope link, and
 *  Stop — the supported interruption, always outside the message rows.
 *  The working state's row presentation itself moved onto the turn's
 *  own name row (`AssistantActivity`), so nothing here re-paints a
 *  full-width loading bar inside the thread. */
function WorkingRow({
  turn,
  appOrigin,
  step,
  resolveAppPath,
}: {
  turn: TurnState;
  appOrigin: MasterAppOrigin | null;
  step: ThreadEntry | null;
  /** CAD-1438: resolves an install id to its unique canonical path (or null). */
  resolveAppPath: (installId: string) => string | null;
}) {
  const t = useTranslate();
  const appHref = appOriginHref(appOrigin, resolveAppPath);
  if (turn.kind === "idle") return null;
  const stepText = step ? stepSummary(step.text) : null;
  return (
    <div className="workrow" role="status" aria-live="polite" data-turn={turn.kind}>
      {turn.kind === "queued" && (
        <span className="text-ink-400 min-w-0">
          {t("Queued")}{turn.summary ? ` — ${stepSummary(turn.summary)}` : ""}
        </span>
      )}
      {turn.kind === "submitting" && <span className="text-ink-400">{t("Sending…")}</span>}
      {turn.kind === "working" && stepText && (
        <span key={stepText} className="stepfade num text-ink-500 truncate min-w-0">
          {stepText}
        </span>
      )}
      {turn.kind === "working" && !stepText && (
        <span className="text-ink-500 min-w-0">{t("Turn in progress")}</span>
      )}
      {turn.kind === "working" && turn.compacting === true && (
        <span className="chip bg-warn/10 text-warn shrink-0">{t("compacting")}</span>
      )}
      {turn.kind === "working" && (turn.queued ?? 0) > 0 && (
        <span className="chip bg-ink-800 text-ink-400 shrink-0">{turn.queued} queued</span>
      )}
      <span className="flex-1" />
      {appHref && (turn.kind === "working" || turn.kind === "queued") && (
        <Link href={appHref} className="lnk text-micro shrink-0">
          {t("Open its conversation")} <span aria-hidden>↗</span>
        </Link>
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
  const t = useTranslate();
  return (
    <div className="cmdcard card px-3 py-2 ml-8 min-w-0" data-cmd={cmd.name}>
      <div className="flex items-center gap-2 text-micro">
        <span className="num text-accent font-medium">
          /{cmd.name}
          {cmd.arg ? ` ${cmd.arg}` : ""}
        </span>
        {cmd.running ? (
          <span className="steps-running text-ink-500">{t("running…")}</span>
        ) : cmd.ok === false ? (
          <span className="text-fail">{t("failed")}</span>
        ) : (
          <span className="text-ink-500">{t("done")}</span>
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
  const t = useTranslate();
  const stopped = status.kind === "stopped";
  return (
    <div className="card px-4 py-4 space-y-3" data-empty="master">
      <h2 className="text-cardtitle font-semibold text-ink-100">
        {hosted === null ? t("Checking assistant availability…") : hosted ? t("Your assistant is unavailable") : stopped ? t("The master is stopped") : t("Start the master to chat")}
      </h2>
      <p className="text-secondary text-ink-400">
        {hosted === null ? t("Checking this board’s session and assistant state.") : hosted ? t("Your workspace administrator can check the assistant’s setup and restore it. You can send messages once it is available.") : t("The master is the agent you talk to here. It plans work with you and hands approved tickets to your agents. Start or resume it from a terminal on this machine:")}
      </p>
      {hosted === false && <pre className="num text-secondary text-ink-200 bg-ink-800 rounded px-3 py-2 overflow-x-auto">
        {stopped ? RESUME_COMMAND : START_COMMAND}
      </pre>}
      <div className="grid sm:grid-cols-2 gap-3 text-label">
        <div>
          <div className="slabel mb-1">{t("it can")}</div>
          <ul className="list-disc pl-4 space-y-0.5 text-ink-300">
            <li>{t("read the tracker, reports and what agents are doing")}</li>
            <li>{t("propose a plan — tickets with size and acceptance")}</li>
            <li>{t("dispatch tickets of a plan you approved")}</li>
            <li>{t("answer workers' questions, and bring you the ones it can't")}</li>
          </ul>
        </div>
        <div>
          <div className="slabel mb-1">{t("it can't")}</div>
          <ul className="list-disc pl-4 space-y-0.5 text-ink-300">
            <li>{t("approve its own plans — only you do, here or with `cadence plan approve`")}</li>
            <li>{t("start work outside an approved plan")}</li>
            <li>{t("merge, push or reach your git credentials")}</li>
            <li>{t("change settings or other agents")}</li>
          </ul>
        </div>
      </div>
    </div>
  );
}

function Examples({ onPick, disabled }: { onPick: (text: string) => void; disabled: boolean }) {
  const t = useTranslate();
  const actor = useAssistantActor();
  return (
    <div className="card px-4 py-4" data-empty="thread">
      <h2 className="text-cardtitle font-semibold text-ink-100">{`${t("Ask")} ${displayName(actor.read, actor.scope, t)}`}</h2>
      <p className="text-secondary text-ink-400 mt-1">{t("No conversation yet. Try one of these:")}</p>
      <div className="mt-3 grid gap-2">
        {EXAMPLES.map((ex) => (
          <button
            key={ex}
            disabled={disabled}
            onClick={() => onPick(ex)}
            className="text-left px-3 py-2 rounded border border-ink-700 text-secondary text-ink-200 hover:border-accent/60 hover:text-accent disabled:opacity-50 disabled:hover:border-ink-700 disabled:hover:text-ink-200"
          >
            {t(ex)}
          </button>
        ))}
      </div>
    </div>
  );
}

const retry = (
  message: string,
  text: string,
  refs?: ThreadRef[],
  attachments?: { id: string }[],
  conversation?: string,
) => sendToMaster(text, message, refs, attachments, conversation);
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
  working,
  stale,
}: {
  items: ThreadItem[];
  limit: number;
  moreBefore: boolean;
  loadingEarlier: boolean;
  onEarlier: () => void;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
  liveAfter: number;
  onRetry: (
    message: string,
    text: string,
    refs?: ThreadRef[],
    attachments?: { id: string }[],
    conversation?: string,
  ) => void;
  /** The live turn: its own `message` id picks the row the activity
   *  belongs to; `since` drives the elapsed clock. Null otherwise. */
  working: { message: string | null; since: number | null } | null;
  /** CAD-1782: the thread stream is down, so these rows are a snapshot. */
  stale: boolean;
}) {
  const t = useTranslate();
  const { shown, hidden } = useMemo(() => visibleWindow(items, limit), [items, limit]);
  const anchors = useMemo(() => planAnchors(items), [items]);
  // The name-row activity's row: this turn's own last item only — a
  // newer pending message or a plan card's anchor never steals it, and
  // it renders independently of PlanCard (separate slots).
  const anchorKey = working ? activityAnchor(shown, working.message) : null;
  // The item the anchored key names: its entry's own timestamp drives
  // the standalone name row (the turn's newest in-flight moment).
  const anchorItem = anchorKey === null ? null : shown.find((i) => i.key === anchorKey) ?? null;
  const anchorEntry: ThreadEntry | null =
    anchorItem === null
      ? null
      : anchorItem.type === "tools"
        ? anchorItem.entries[anchorItem.entries.length - 1] ?? null
        : anchorItem.type === "pending"
          ? null
          : anchorItem.entry;
  // A daemon that cannot name its running turn's message still owes an
  // honest Working signal, and so does a named turn whose first stored
  // entry has not landed yet: the bounded fallback rides the tail item —
  // including an answer tail that belongs to an EARLIER turn (a stale or
  // failed reply). That answer keeps its own header slot, and the
  // running turn gets a standalone row after it, so a turn is never
  // invisible in the transcript while the dock counts it working
  // (CAD-1533).
  const fallbackItem =
    working && anchorItem === null && shown.length > 0 ? shown[shown.length - 1] : null;
  const activityItem = anchorItem ?? fallbackItem;
  if (items.length === 0 && !moreBefore) return null;
  return (
    <>
      {stale && <StreamBanner onRetry={() => window.location.reload()} />}
      {(hidden > 0 || moreBefore) && (
        <div className="flex justify-center">
          <button className="lnk text-label disabled:opacity-50" disabled={loadingEarlier} onClick={onEarlier}>
            {loadingEarlier ? t("Loading…") : t("Load earlier messages")}
          </button>
        </div>
      )}
      <Conversation
        mode={{ kind: "home" }}
        items={shown}
        readOnly={readOnly}
        liveAfter={liveAfter}
        running={working?.message ?? null} /* null while a send is only submitting */
        legacyTranscript={!items.some((item) => item.type === "answer")}
        onOpenIssue={onOpenIssue}
        onRetry={onRetry}
        onDiscard={discard}
        activity={(item) => {
          if (!working || activityItem === null || item.key !== activityItem.key) return null;
          const live = <AssistantActivity since={working.since} />;
          // The anchored row shows this turn's own newest entry time. A
          // fallback row rides the tail item, which may belong to an
          // EARLIER turn (a stale or failed reply), so it is stamped with
          // the running turn's start — never with that foreign time
          // (CAD-1533 review, finding 1).
          const at =
            anchorItem !== null
              ? anchorEntry
                ? time(anchorEntry.created)
                : undefined
              : working.since !== null
                ? time(new Date(working.since * 1000).toISOString())
                : undefined;
          // The answer bubble carries the indicator on its own name row;
          // a standalone row covers in-flight steps/commentary that have
          // none of their own, and the running turn when the row it would
          // anchor to is another turn's reply (CAD-1533). A turn with no
          // stored entry yet has nothing to anchor to, so it shows the
          // inline typing bubble after the operator's own message instead.
          if (item.type === "answer" && anchorItem !== null) return { kind: "header", node: live };
          const below = item.type === "operator" || item.type === "pending" || item.type === "answer";
          const needsTypingTurn =
            anchorItem === null || item.type === "operator" || item.type === "pending";
          return {
            kind: "row",
            node: needsTypingTurn ? <ThreadTurn items={[]} running /> : <AssistantNameRow at={at} live={live} />,
            position: below ? "after" : "before",
            ...(anchorItem === null ? { fallback: true as const } : {}),
          };
        }}
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
/**
 * CAD-1485: the outer Home owns the mode decision only. A signed-in
 * member (or an explicit `?chat=assistant` on a hosted board) gets the
 * private-assistant surface; everyone else gets the Master workspace.
 * The two are siblings — MemberChat never mounts MasterHome's hooks,
 * so a member never subscribes, reads or sends on any Master resource.
 */
export default function Home({
  readOnly,
  hosted = null,
  viewerSub = null,
  viewerIsMember = false,
  sessionId = null,
  overview,
  onOpenIssue,
  onOpenDecision,
  overviewHref,
  permissionsHref,
  resolveAppPath,
  setupNotice = null,
  operator = null,
}: {
  readOnly: boolean;
  hosted?: boolean | null;
  /** CAD-1817: the verified session proves operator authority, the only
   *  viewer that may edit the workspace coordinator's identity. `null`
   *  is unproven, never an operator. */
  operator?: boolean | null;
  /** CAD-1485: the verified public session's subject — member or
   *  operator/owner — when the session names one. `?chat=assistant`
   *  keys the owner's private chat on it; a member's default view is
   *  already private. */
  viewerSub?: string | null;
  /** The verified session's `role === "member"`. Members use the private
   *  assistant layout; owners/operators keep the master-transport layout
   *  unless they select the private layout. The backend still determines
   *  the actual conversation actor. */
  viewerIsMember?: boolean;
  /** The verified session's id — scopes the member-chat store so a
   *  re-login as another member never reads the previous chat. */
  sessionId?: string | null;
  overview: ResourceState<Overview>;
  onOpenIssue: (id: string) => void;
  /** Open a reading item in the shared work-item detail (App owns the mount). */
  onOpenDecision: (open: HomeDecisionOpen) => void;
  overviewHref: string;
  /** Settings → Master permissions (the standing "Always" rules). */
  permissionsHref: string;
  /** CAD-1438: resolves an install id to its unique canonical `/apps/<key>` path, or null. */
  resolveAppPath: (installId: string) => string | null;
  /** The first-run setup strip (CAD-1312): rendered inside the chat
   *  panel below the Assistant heading so the Needs-you rail beside it
   *  keeps the workspace's top edge. */
  setupNotice?: ReactNode;
}) {
  // `?chat=assistant` is a UI-mode selector only — it names the signed-in
  // user's OWN private assistant, never another actor. It changes no actor
  // and no authority. The private surface is a HOSTED-board feature: a
  // local or tailnet board's named session (device grant, unsigned local
  // sign-in) never enters it — those keep the Master workspace, exactly
  // like a standalone board. On a hosted board a member lands on their own
  // assistant by default; an owner reaches theirs only through the query.
  const chatParam = new URLSearchParams(window.location.search).get("chat") === "assistant";
  const memberChat = hosted === true && viewerSub !== null && (viewerIsMember || chatParam);
  if (memberChat && viewerSub !== null) {
    const scope = memberScope(sessionId, viewerSub);
    return (
      <MemberChat
        key={scope}
        scope={scope}
        readOnly={readOnly}
        onOpenIssue={onOpenIssue}
      />
    );
  }
  return (
    <MasterHome
      readOnly={readOnly}
      hosted={hosted}
      overview={overview}
      onOpenIssue={onOpenIssue}
      onOpenDecision={onOpenDecision}
      overviewHref={overviewHref}
      permissionsHref={permissionsHref}
      resolveAppPath={resolveAppPath}
      setupNotice={setupNotice}
      operator={operator}
    />
  );
}

/** The shared master-transport pane: Native coordinator or verified hosted
 *  actor. The outer Home chooses the layout without changing recipients. */
function MasterHome({
  readOnly,
  hosted = null,
  overview,
  onOpenIssue,
  onOpenDecision,
  overviewHref,
  permissionsHref,
  resolveAppPath,
  setupNotice = null,
  operator = null,
}: {
  readOnly: boolean;
  hosted?: boolean | null;
  operator?: boolean | null;
  overview: ResourceState<Overview>;
  onOpenIssue: (id: string) => void;
  onOpenDecision: (open: HomeDecisionOpen) => void;
  overviewHref: string;
  permissionsHref: string;
  resolveAppPath: (installId: string) => string | null;
  setupNotice?: ReactNode;
}) {
  const t = useTranslate();
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
  const [link, setLink] = useState<SseErrorState | "live" | null>("reconnecting");
  const [limit, setLimit] = useState(WINDOW);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  const [cmds, setCmds] = useState<CmdResult[]>([]);
  const [unseen, setUnseen] = useState(0);
  // Same-row growth while the reader is in history (no count to show).
  const [activity, setActivity] = useState(false);
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
  // The default master-state response already resolves Home before its first
  // transcript exists. Keep that server-issued binding when the empty thread
  // (or its first streamed entries) does not carry a thread ID yet.
  const homeProjection = master.status === "ok" ? master.data?.actor_identity : null;
  const homeConversation = thread.data?.conversation
    ?? (homeProjection?.scope === "personal"
      && typeof homeProjection.conversation_id === "string"
      && readConversationIdentity(master.data, homeProjection.conversation_id).read.state === "ready"
      ? homeProjection.conversation_id
      : undefined);
  useEffect(() => {
    if (homeConversation === undefined || thread.data === null
      || thread.data.conversation !== undefined
      || resources.masterState.get().data !== master.data) return;
    // Stream admission and turn controls use the same canonical binding.
    // Never replace a transcript's ID or seed a superseded session's store.
    resources.masterThread.mutate((current) => current.conversation === undefined
      ? { ...current, conversation: homeConversation }
      : current);
  }, [homeConversation, thread.data, master.data]);
  const attachDestination: AttachDestination | undefined = loaded && !missing
    ? homeConversation ? { conversation: homeConversation } : {}
    : undefined;
  const canAttach = !readOnly && attachDestination !== undefined;

  const masterRow = agents.data?.agents.find((a) => a.alias === MASTER);
  const submitting = (thread.data?.pending ?? []).some((p) => p.state !== "failed");
  const turn = turnState(master.data, masterRow, submitting);
  // The turn bar (turnBar.ts) is the same derivation Conversation uses, so
  // the two surfaces cannot disagree: Stop only for the running turn's own
  // message, Cancel only for this surface's own queued message. Home uses
  // the server-issued thread ID, just as app conversations do.
  const threadEntries = thread.data?.entries ?? [];
  const permissionPending = usePendingPermission(threadEntries);
  // Results older than this Home visit are history, not a live outcome.
  const liveSince = useMemo(() => Date.now(), []);
  const streamReconnecting = link !== null && link !== "live";
  const turnBar = turnBarState(
    {
      master: master.data,
      entries: threadEntries,
      pendingMessages: (thread.data?.pending ?? []).filter((p) => p.state !== "failed").map((p) => p.message),
      awaitingApproval: permissionPending,
      reconnecting: streamReconnecting,
      liveSince,
    },
    homeConversation ?? null,
  );
  // The Pi watch closes normally between turns; the SPA reconnects with a
  // fresh cursor. That is not a stale conversation while no turn is active.
  // `writing` and `approval` are presentations of a running turn; `sending`
  // is a message already in flight. All three still need the outage notice.
  const activeTurn =
    turnBar.kind === "working" ||
    turnBar.kind === "writing" ||
    turnBar.kind === "approval" ||
    turnBar.kind === "sending" ||
    turnBar.kind === "queued";
  // CAD-1880: Stop sits in the composer's send slot while this pane's own
  // turn runs. Never while the stream is down: that state is last known.
  const runningMessage =
    turnBar.kind === "working" || turnBar.kind === "writing" ? turnBar.stopMessage : null;
  const canStop = writeBlock === null && !streamReconnecting && runningMessage !== null;
  const aheadHref = useConversationDestination(
    turnBar.kind === "queued" ? turnBar.aheadConversation : null,
    master.data,
    homeConversation,
  );
  const failedEntry = turnBar.kind === "failed" && turnBar.message !== null
    ? threadEntries.find((entry) => entry.role === "operator" && entry.message === turnBar.message) ?? null
    : null;
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
  useCanvasFeed({
    conversation: homeConversation ?? null,
    surface: "home",
    composerKey: "home",
    items,
    loaded: loaded && !missing,
    // The turn must be this conversation's own: an app-origin turn is not.
    running: turn.kind === "working" && turnOrigin === null ? turn.message : null,
  });
  // The running turn's name-row activity anchors to this turn's own
  // message id — the item holding its newest stored entry (a pending
  // row has no stored entry yet, so its row is the anchor while it
  // waits). Once the terminal `turn_result` lands the daemon flips
  // `turn.state` off working and the indicator clears with the reply.
  // The inline typing bubble starts the moment the send begins: `submitting`
  // is the composer's own POST in flight (no stored entry to anchor on yet),
  // `working` the daemon's verified turn. A queued wait keeps the dock's
  // honest "behind a turn" line instead of claiming to type.
  const workingActivity =
    turn.kind === "working"
      ? { message: turn.message, since: typeof turn.since === "number" ? turn.since : null }
      : turn.kind === "submitting"
        ? { message: null, since: null }
        : null;
  // The ResizeObserver re-subscribes when the name-row indicator appears
  // or clears (its content lives inside the observed list): a running
  // turn with any row to ride, whether it anchors to its own entry or
  // falls back to any tail item — answers included (CAD-1533 review,
  // finding 2).
  const nameRowShown = workingActivity !== null && items.length > 0;

  // Live entries: the store opened on the newest page, so the stream
  // resumes after its last seq — never a replay of the history. On a
  // reconnect `streamInto` resumes with `?after=<last id>` and the
  // reducer dedupes by seq: no gaps, no repeats.
  useThreadStream(
    resources.masterThread,
    `/api/threads/${MASTER}/stream`,
    loaded && !missing && (master.data === null
      || !Object.hasOwn(master.data, "actor_identity")
      || thread.data?.conversation !== undefined),
    setLink,
  );

  // The seq watermark for the enter animation: items at or below the
  // mount tail are history — they render still; only what lands after
  // rises in.
  useEffect(() => {
    if (liveAfter.current === null && loaded) {
      liveAfter.current = lastSeq(thread.data) ?? 0;
    }
  }, [loaded, thread.data]);

  // Keep master_state honest: it follows the agents resource's
  // invalidations (turn boundaries land there) and the `master` stream
  // frame (CAD-1700: turn, queue, tool steps and permissions in any
  // conversation). A daemon that answered 501 (no master_state) stops the
  // follow — send/command refreshes still retry it explicitly.
  useEffect(() => {
    if (master.status !== "failed") void resources.masterState.refresh();
  }, [agents.data, master.status]);
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
  // Opening Home marks its complete latest reply watermark, not a bounded tail.
  const homeReplySeq = thread.data?.replySeq ?? 0;
  useEffect(() => {
    if (homeReplySeq > 0) noteHomeSeen(homeReplySeq);
  }, [homeReplySeq]);

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
    setActivity(false);
    setGlideTick((n) => n + 1);
  }, []);
  const sendPersonalRequest = useCallback(async (text: string) => {
    if (readOnly || block || homeConversation === undefined) throw new Error("Sending is unavailable.");
    engage();
    const receipt = await sendToMaster(text, newMessageId(), undefined, undefined, homeConversation);
    if (!receipt.ok) throw new Error(receipt.error);
  }, [readOnly, block, homeConversation, engage]);
  const actor = useConversationActor(
    master,
    operator === true && !readOnly,
    homeConversation ?? null,
    readOnly || !!block || homeConversation === undefined ? null : sendPersonalRequest,
  );
  const onRetrySend = useCallback(
    (
      message: string,
      text: string,
      refs?: ThreadRef[],
      attachments?: { id: string }[],
      conversation?: string,
    ) => {
      engage();
      retry(message, text, refs, attachments, conversation);
    },
    [engage],
  );
  const onPanelScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const near = atTail(el.scrollTop, el.scrollHeight, el.clientHeight);
    if (near) setActivity(false);
    applyFollow(onViewportScroll(follow.current, near));
  }, []);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const nearNow = () => atTail(el.scrollTop, el.scrollHeight, el.clientHeight);
    const onScrollEnd = () => {
      const settled = onScrollSettled(follow.current, nearNow());
      applyFollow(settled.state);
      if (settled.state.pinned) setActivity(false);
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

  // Same-row growth (a streamed reply, a tool step, the name row's
  // working line) keeps `items.length` and `lastKey` unchanged, so the
  // tail effect below does not run for it. Observe the thread's own
  // content box and follow only while pinned: a reader in history keeps
  // its position and the pill counts nothing here.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let last = el.scrollHeight;
    const ro = new ResizeObserver(() => {
      const h = el.scrollHeight;
      const grew = h > last;
      last = h;
      if (!grew || follow.current.programmatic) return;
      if (follow.current.pinned) scrollPanel("jump");
      else setActivity(true);
    });
    // Observe every direct child so growth in any block (the reply row,
    // the working line, a tool group) reaches the observer.
    for (const child of Array.from(el.children)) ro.observe(child);
    return () => ro.disconnect();
  }, [scrollPanel, items.length, nameRowShown, turn.kind, cmds.length]);

  // The scroller's own box changing height (a shorter window, a notice
  // mounting above it) can leave the tail under the dock: on any change
  // while the pinned flag is set, jump back to the tail; a reader who
  // scrolled up is not pinned and keeps their place.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el || typeof ResizeObserver === "undefined") return;
    let prev = el.clientHeight;
    const ro = new ResizeObserver(() => {
      const next = el.clientHeight;
      const repin = repinOnResize(follow.current.pinned, prev, next);
      prev = next;
      if (repin) scrollPanel("jump");
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [scrollPanel]);

  // The dock's measured height is the thread's bottom padding and the
  // pill's floor (`--dock-h`, styles.css); while pinned, a growing dock
  // (a longer draft, a wrapped hint) keeps the tail in view.
  useEffect(() => {
    const dock = dockRef.current;
    if (!dock || typeof ResizeObserver === "undefined") return;
    const apply = () => {
      // Judge "on the tail" before the new padding lands: a strip that
      // appears (Queued) must not push the last bubble under the dock.
      const el = scrollRef.current;
      const onTail = el ? atTail(el.scrollTop, el.scrollHeight, el.clientHeight) : false;
      document.documentElement.style.setProperty("--dock-h", `${dock.offsetHeight}px`);
      if (follow.current.pinned || onTail) scrollPanel("jump");
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
    setActivity(false);
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

  const runTurnCommand = useCallback((command: "stop" | "cancel", message: string) => {
    if (writeBlock !== null) return;
    const id = ++cmdSeq.current;
    api
      .masterCommand(command, undefined, undefined, message)
      .then((r) => {
        void resources.masterState.refresh();
        if (!r.ok) setCmds((c) => [...c, { id, name: command, ok: false, text: "The master did not accept the command." }]);
      })
      .catch((e: ApiError) => setCmds((c) => [...c, { id, name: command, ok: false, text: e.message }]));
  }, [writeBlock]);

  const onEarlier = useCallback(() => {
    const step = earlierStep(resources.masterThread, threadReader(MASTER), limit);
    if (step === null || step.kind === "complete") return;
    if (step.kind === "widen") {
      setLimit((l) => l + WINDOW);
      return;
    }
    setLoadingEarlier(true);
    step.page
      .then((older) => {
        earlierLanded.current = true;
        step.land(older);
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
    <AssistantActorProvider actor={actor}>
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
          onOpenDecision={onOpenDecision}
          onAskAgent={(agent, update) => setSeed((s) => ({
            text: `Check in with ${agent.alias}${agentOn(agent).length ? ` on ${agentOn(agent).join(", ")}` : ""}. ${update ? `Their latest update: ${update.label}${update.text ? ` — ${update.text.slice(0, 1200)}` : ""}. ` : ""}What is the current progress, and does anything need my input?`,
            refs: [], n: s.n + 1,
          }))}
          collapsed={railCollapsed}
          onToggleCollapse={toggleRail}
          onSent={engage}
        />
      </aside>

      <section className="min-w-0 rail:order-1 flex flex-col min-h-0 gap-4" aria-label={t("master thread")}>
        <SinceCard onOpenIssue={onOpenIssue} hosted={hosted} />

        {/* CAD-600: the conversation panel — the master's header, the
            thread scrolling inside it, and the composer docked to its
            bottom. It fills the height below the board header; the page
            itself never scrolls for the chat. */}
        <div className="relative flex-1 min-h-0 flex flex-col" data-chat-panel>
          <div
            className="master-heading assistant-heading pb-2.5 border-b border-ink-700/70"
            data-assistant-identity={actor.scope ?? undefined}
            data-identity-state={actor.read.state}
          >
            <AssistantFace size={30} active={status.kind === "running"} />
            <div className="assistant-heading-text">
              <div className="assistant-heading-name">
                <h1 className="text-section font-semibold text-ink-100" data-assistant-name>{displayName(actor.read, actor.scope, t)}</h1>
                <span
                  className={`chip ${
                    status.kind === "running"
                      ? "bg-ok/15 text-ok"
                      : status.kind === "unknown"
                        ? "bg-ink-800 text-ink-400"
                        : "bg-warn/10 text-warn"
                  }`}
                >
                  {status.kind === "running" ? status.state : status.kind === "absent" ? t("not started") : status.kind}
                </span>
                <MasterChips master={master.data} row={masterRow} readOnly={readOnly} />
                <HeaderMenu master={master.data} row={masterRow} readOnly={readOnly} scope={scopeDescriptor(actor.scope, t)} />
                {streamReconnecting && activeTurn && status.kind === "running" && (
                  <span className="text-micro text-ink-500" data-stale>{link === "stopped" ? t("stream stopped") : t("reconnecting…")}</span>
                )}
              </div>
              <p className="assistant-scope">{scopeDescriptor(actor.scope, t)}</p>
            </div>
            <div className="assistant-heading-actions">
              <AssistantAboutButton />
            </div>
          </div>

          {setupNotice}

          {thread.status === "failed" && (
            <div className="card px-3.5 py-3 mt-3 text-label text-fail break-words" role="alert">
              {t("The thread could not be read —")} {thread.error}{" "}
              <button className="lnk" onClick={() => void resources.masterThread.refresh()}>
                {t("Retry")}
              </button>
            </div>
          )}
          {!loaded && thread.status !== "failed" && (
            <p className="text-label text-ink-500 pt-3">{t("Reading the thread…")}</p>
          )}

          <div
            className="chat-scroll min-h-0 flex-1 overflow-y-auto pt-3"
            ref={scrollRef}
            onScroll={onPanelScroll}
            data-chat-scroll
          >
            <CloudContentNotice withheld={thread.data?.cloudWithheld} />
            {thread.data?.cloudWithheld == null && showNotStarted && items.length === 0 && (
              <div className="chat-empty">
                <NotStarted status={status} hosted={hosted} />
              </div>
            )}
            {thread.data?.cloudWithheld == null && empty && !showNotStarted && (
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
              working={workingActivity}
              stale={streamReconnecting && activeTurn}
            />
            {cmds.map((c) => (
              <CmdCard key={c.id} cmd={c} onOpenIssue={onOpenIssue} />
            ))}
          </div>

          {/* The status node stays mounted so a screen reader announces the
              pill's arrival (a live region added with its text is not). */}
          <span role="status" aria-live="polite" className="sr-only">
            {showJumpPill(unseen, activity) ? jumpLabel(unseen, t) : ""}
          </span>
          {showJumpPill(unseen, activity) && (
            <button type="button" className="newpill num" onClick={jumpToLatest} data-jump-to-latest>
              <span aria-hidden="true">↓</span> {jumpLabel(unseen, t)}
            </button>
          )}

          <div className="chatdock" ref={dockRef} data-chat-dock>
            {/* The turn bar (CAD-1700) owns the turn's state and its Stop and
                Cancel; the working presentation itself is the subtle name-row
                indicator (CAD-1462). */}
            <TurnBar
              state={turnBar}
              reconnecting={streamReconnecting && activeTurn}
              actions={{
                onCancel: writeBlock === null ? (message) => runTurnCommand("cancel", message) : undefined,
                onRetry:
                  writeBlock === null && failedEntry !== null
                    ? () => {
                        engage();
                        const payload = failedEntry.payload ?? {};
                        const list = (key: string) => (Array.isArray(payload[key]) ? (payload[key] as unknown[]) : []);
                        const refs = list("refs").flatMap((item) => {
                          const r = item as { kind?: unknown; id?: unknown };
                          return typeof r.kind === "string" && typeof r.id === "string"
                            ? [{ kind: r.kind, id: r.id }]
                            : [];
                        });
                        const attachments = list("attachments").flatMap((item) => {
                          const a = item as { id?: unknown };
                          return typeof a.id === "string" ? [{ id: a.id }] : [];
                        });
                        const mediaRefs = list("media_refs").flatMap((item) => {
                          if (!item || typeof item !== "object") return [];
                          const ref = item as Record<string, unknown>;
                          return typeof ref.asset_id === "string" && typeof ref.revision === "number" && typeof ref.sha256 === "string"
                            ? [{ asset_id: ref.asset_id, revision: ref.revision, sha256: ref.sha256 }]
                            : [];
                        });
                        const conversation =
                          typeof payload.conversation === "string" ? payload.conversation : undefined;
                        void sendToMaster(failedEntry.text, newMessageId(), refs, attachments.length ? attachments : undefined, conversation, undefined, mediaRefs.length ? mediaRefs : undefined);
                      }
                    : undefined,
                onView: turnBar.kind === "queued" && aheadHref !== null
                  ? aheadHref === "/" ? jumpToLatest : () => window.location.assign(aheadHref)
                  : undefined,
                onReview: () => {
                  const card = scrollRef.current?.querySelector<HTMLElement>("[data-permission-status='pending']");
                  card?.scrollIntoView({ block: "center", behavior: motionBehavior(reducedMotion(), "jump") });
                },
              }}
            />
            {/* Another conversation's turn (an app's), with its link: Home's
                own turn is the turn bar above. */}
            {turnBar.kind === "idle" && (
              <WorkingRow
                turn={turn}
                appOrigin={turnOrigin}
                step={step}
                resolveAppPath={resolveAppPath}
              />
            )}
            <Composer
              block={block}
              seed={seed}
              onCommand={runCommand}
              onSent={engage}
              stop={canStop ? { onStop: () => { if (runningMessage) runTurnCommand("stop", runningMessage); } } : null}
              attachEnabled={canAttach}
              attachDestination={attachDestination}
              attachReason={readOnly
                ? "The board is read-only."
                : missing
                  ? "The Master conversation is unavailable."
                  : "A Home conversation must be ready before attaching files."}
            />
          </div>
        </div>
      </section>
    </main>
    </AssistantActorProvider>
  );
}
