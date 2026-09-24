import { memo, useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { api, ApiError } from "../../lib/api";
import type { ResourceState } from "../../lib/cache";
import { resources, threadReader } from "../../lib/resources";
import { streamInto, type SseErrorState } from "../../lib/sse";
import { useQuery, useResource } from "../../lib/useResource";
import type { Overview } from "../../lib/types";
import Md from "../../ui/Md";
import { composerBlock, MASTER, masterStatus, START_COMMAND, type MasterStatus } from "./master";
import NeedsRail from "./NeedsRail";
import PlanCard from "./PlanCard";
import SinceCard from "./SinceCard";
import {
  addPending,
  discardPending,
  lastSeq,
  applyEarlier,
  fetchEarlier,
  newMessageId,
  planAnchors,
  reduceFrame,
  settlePending,
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

function time(created: string | null | undefined): string {
  if (!created) return "";
  const d = new Date(created);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** A run of tool calls and results: one collapsed line, expandable. */
function ToolLines({ entries }: { entries: ThreadEntry[] }) {
  const calls = entries.filter((e) => e.kind === "tool_call").length;
  const failed = entries.some((e) => e.payload?.is_error === true);
  const last = entries[entries.length - 1];
  return (
    <details className="group ml-8 min-w-0" data-kind="tools">
      <summary className="list-none cursor-pointer flex items-center gap-1.5 text-micro text-ink-500 hover:text-ink-300 min-w-0">
        <span className="num shrink-0 group-open:rotate-90 transition-transform">›</span>
        <span className="shrink-0">
          {calls || entries.length} tool {(calls || entries.length) === 1 ? "step" : "steps"}
        </span>
        {failed && <span className="text-fail shrink-0">· error</span>}
        <span className="num truncate min-w-0">· {last.text}</span>
      </summary>
      <ul className="mt-1 space-y-0.5 border-l border-ink-700 pl-2.5">
        {entries.map((e) => (
          <li
            key={e.seq}
            className={`num text-micro break-all ${
              e.payload?.is_error === true ? "text-fail" : e.kind === "tool_call" ? "text-ink-400" : "text-ink-500"
            }`}
          >
            {e.kind === "tool_call" ? "→ " : "← "}
            {e.text}
          </li>
        ))}
      </ul>
    </details>
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
  onRetry: (message: string, text: string) => void;
  onDiscard: (message: string) => void;
}) {
  switch (item.type) {
    case "operator":
      return (
        <Bubble who="You" at={time(item.entry.created)} me>
          <div className="inline-block text-left mt-0.5 px-3 py-2 rounded-lg bg-accent/10 text-body text-ink-100 whitespace-pre-wrap break-words">
            {item.entry.text}
          </div>
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
          {item.pending.state === "failed" && (
            <div className="text-micro text-fail mt-1 break-words">
              {item.pending.error}{" "}
              <button className="lnk" onClick={() => onRetry(item.pending.message, item.pending.text)}>
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
      return <ToolLines entries={item.entries} />;
    case "answer":
      return (
        <Bubble who="Master" at={time(item.entry.created)}>
          <div className="issue-reader mt-0.5 text-body text-ink-200 break-words" data-kind="answer">
            <Md text={item.entry.text} onOpen={onOpenIssue} />
          </div>
        </Bubble>
      );
    case "system":
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

/** Queue `text` to the master: optimistic entry, reconciled by message id. */
function sendToMaster(text: string, message = newMessageId()): void {
  const body = text.trim();
  if (!body) return;
  const store = resources.masterThread;
  store.write((s) => addPending(s, message, body, Date.now()));
  api
    .threadSend(MASTER, body, message)
    .then(() => store.write((s) => settlePending(s, message, { ok: true })))
    .catch((e: ApiError) =>
      store.write((s) => settlePending(s, message, { ok: false, error: e.message ?? String(e) })),
    );
}

const retry = (message: string, text: string) => sendToMaster(text, message);
const discard = (message: string) => resources.masterThread.write((s) => discardPending(s, message));

/**
 * The composer owns its draft: typing re-renders this form only, never
 * the thread above it. `seed` (an example ask) replaces the draft.
 */
function Composer({ block, seed }: { block: string | null; seed: { text: string; n: number } }) {
  const [draft, setDraft] = useState("");
  const form = useRef<HTMLFormElement>(null);
  useEffect(() => {
    if (seed.n > 0) setDraft(seed.text);
  }, [seed]);
  // The composer is sticky on wide screens: keep scrolled-to controls
  // (a plan card's buttons, a focused field) clear of it by reserving
  // its height as the page's bottom scroll padding.
  useEffect(() => {
    const el = form.current;
    const root = document.documentElement;
    if (!el || typeof ResizeObserver === "undefined") return;
    const apply = () => {
      const sticky = getComputedStyle(el).position === "sticky";
      root.style.scrollPaddingBottom = sticky ? `${el.offsetHeight + 24}px` : "";
    };
    const ro = new ResizeObserver(apply);
    ro.observe(el);
    addEventListener("resize", apply);
    apply();
    return () => {
      ro.disconnect();
      removeEventListener("resize", apply);
      root.style.scrollPaddingBottom = "";
    };
  }, []);
  const submit = () => {
    if (!draft.trim() || block) return;
    sendToMaster(draft);
    setDraft("");
  };
  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      submit();
    }
  };
  return (
    <form
      ref={form}
      className="card p-2.5 lg:sticky lg:bottom-3"
      data-composer
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <textarea
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={onKey}
        rows={2}
        disabled={!!block}
        placeholder={block ? "" : "Ask the master for work…"}
        aria-label="message to the master"
        className="w-full resize-y bg-transparent text-body text-ink-100 placeholder:text-ink-500 outline-none disabled:opacity-50 min-h-[2.75rem]"
      />
      <div className="flex items-center gap-2 mt-1.5">
        <p className="text-micro text-ink-500 min-w-0 flex-1 break-words" data-composer-block={block ? "" : undefined}>
          {block ?? "Enter sends · Shift+Enter for a new line"}
        </p>
        <button
          type="submit"
          disabled={!!block || !draft.trim()}
          className="h-8 px-3 rounded bg-accent text-on-accent text-label font-medium disabled:opacity-40 shrink-0"
        >
          Send
        </button>
      </div>
    </form>
  );
}

/**
 * The rendered thread: a bounded window of the newest items (a long
 * thread renders at most `limit`), with "load earlier" above it. Memoized
 * — it re-renders when the thread or the window changes, not on
 * unrelated Home state.
 */
const ThreadList = memo(function ThreadList({
  items,
  limit,
  moreBefore,
  loadingEarlier,
  onEarlier,
  readOnly,
  onOpenIssue,
}: {
  items: ThreadItem[];
  limit: number;
  moreBefore: boolean;
  loadingEarlier: boolean;
  onEarlier: () => void;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
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
        {shown.map((item) => (
          <li key={item.key} className="min-w-0 space-y-2">
            <Item item={item} onOpenIssue={onOpenIssue} onRetry={retry} onDiscard={discard} />
            {anchors.has(item.key) && (
              <div className="ml-8">
                <PlanCard epic={anchors.get(item.key)!} readOnly={readOnly} onOpenIssue={onOpenIssue} />
              </div>
            )}
          </li>
        ))}
      </ol>
    </>
  );
});

/**
 * Home (CAD-328): the master's thread with a composer, the "since you
 * left" card above it, and the Needs-you rail beside it (above it on a
 * phone).
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
  const [seed, setSeed] = useState({ text: "", n: 0 });
  const [link, setLink] = useState<SseErrorState | "live" | null>(null);
  const [limit, setLimit] = useState(WINDOW);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  const bottom = useRef<HTMLDivElement>(null);

  const missing = thread.data?.missing === true;
  const status = masterStatus(agents.data, missing);
  const block = composerBlock(readOnly, status);
  const items = useMemo(() => threadItems(thread.data), [thread.data]);
  const loaded = thread.data !== null;
  const moreBefore = thread.data?.moreBefore === true;

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

  // Follow the conversation as it grows at the bottom (not when earlier
  // messages are loaded above).
  const lastKey = items.length ? items[items.length - 1].key : "";
  useEffect(() => {
    if (lastKey) bottom.current?.scrollIntoView?.({ block: "end" });
  }, [lastKey]);

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
        resources.masterThread.write((cur) => applyEarlier(cur, older));
        setLimit((l) => l + WINDOW);
      })
      .catch(() => undefined)
      .finally(() => setLoadingEarlier(false));
  }, [limit]);

  const showNotStarted = status.kind === "absent" || status.kind === "stopped";
  const empty = loaded && !missing && items.length === 0;

  return (
    <main className="px-4 lg:px-8 pt-5 pb-6 w-full min-w-0 grid gap-5 lg:grid-cols-[minmax(0,1fr)_minmax(0,20rem)] lg:items-start">
      <aside className="min-w-0 lg:order-2 lg:sticky lg:top-[3.6rem] space-y-4">
        <NeedsRail overview={overview} readOnly={readOnly} onOpenIssue={onOpenIssue} overviewHref={overviewHref} />
      </aside>

      <section className="min-w-0 lg:order-1 flex flex-col gap-4" aria-label="master thread">
        <SinceCard onOpenIssue={onOpenIssue} />

        <div className="flex items-center gap-2">
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
          {link && link !== "live" && status.kind === "running" && (
            <span className="text-micro text-ink-500">{link === "stopped" ? "stream stopped" : "reconnecting…"}</span>
          )}
        </div>

        {thread.status === "failed" && (
          <div className="card px-3.5 py-3 text-label text-fail break-words" role="alert">
            The thread could not be read — {thread.error}{" "}
            <button className="lnk" onClick={() => void resources.masterThread.refresh()}>
              Retry
            </button>
          </div>
        )}
        {!loaded && thread.status !== "failed" && <p className="text-label text-ink-500">Reading the thread…</p>}
        {showNotStarted && items.length === 0 && <NotStarted status={status} />}
        {empty && !showNotStarted && (
          <Examples onPick={(t) => setSeed((s) => ({ text: t, n: s.n + 1 }))} disabled={!!block} />
        )}

        <ThreadList
          items={items}
          limit={limit}
          moreBefore={moreBefore}
          loadingEarlier={loadingEarlier}
          onEarlier={onEarlier}
          readOnly={readOnly}
          onOpenIssue={onOpenIssue}
        />
        <div ref={bottom} />

        <Composer block={block} seed={seed} />
      </section>
    </main>
  );
}
