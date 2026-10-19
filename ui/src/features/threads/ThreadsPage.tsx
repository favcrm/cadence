import { useCallback, useEffect, useId, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { ApiError, api } from "../../lib/api";
import { onPrivateSessionReset, withPrivateOperation } from "../../lib/privateSession";
import { resources } from "../../lib/resources";
import { useResource } from "../../lib/useResource";
import { navigate, useHref } from "../../lib/useLocation";
import { fmtTime } from "../../lib/fmt";
import { useWriteBlock } from "../auth/WriteGate";
import { useTranslate } from "../../lib/locale";
import { displayUiNotice, type UiNotice } from "../../lib/uiNotice";
import Button from "../../ui/Button";
import Select from "../../ui/Select";
import NewHumanConversation from "./NewHumanConversation";
import Link from "../../ui/Link";
import PageState from "../../ui/PageState";
import { issuePath } from "../issues/model";
import { IconClose, IconRefresh, IconSearch, IconThreads } from "../../ui/icons";
import type { Agent } from "../../lib/types";
import Composer from "../app-shell/chat/Composer";
import { asEntry, newMessageId, type ThreadEntry } from "../home/thread";
import AgentAvatar, { lookFor } from "../agents/AgentAvatar";
import { activeTasks } from "../agents/AgentBlocks";
import { lifecycleBadge, lifecycleOf } from "../agents/agentView";
import { StageLabel, ThreadComposerFrame, Turn } from "./ThreadChatParts";
import {
  changeSubscriber,
  createTopic,
  listWorkConversations,
  postToTopic,
  readTopic,
  readWorkConversation,
  requestFromTopic,
  type ConversationListFilter,
  type ConversationRow,
  type DeliveryState,
  type Participant,
  type PostDelivery,
  type ReadPageOptions,
  type ThreadMessage,
  type TopicPost,
  predatesThreads,
  subscribeThreadsChanged,
  threadsRevisionNow,
} from "./threadsClient";
import {
  conversationTitle,
  dmExpectation,
  kindFilterMatches,
  operatorDmAlias,
  participantLabel,
  readThreadsUrl,
  threadsUrlSearch,
  type ThreadsUrlState,
} from "./threadsView";
import {
  HumanContextPanel,
  HumanRoomRows,
  HumanRoomTimeline,
  useHumanAgents,
  useHumanMe,
  useHumanPeople,
  useHumanRooms,
  type HumanRoomState,
} from "./HumanThreads";
import "./threads.css";

/**
 * Threads (CAD-818): one board page for agent, operator and group
 * conversations. Inbox variant 1: list | timeline | participants rail.
 * The participant roster lives only in the rail; below the desktop width
 * the same panel opens in a native dialog (Context).
 *
 * Authority is the daemon's. The page only issues operator requests and
 * shows what the daemon answers. Agent pairs are read-only (Reply opens the
 * agent's own page). A group post notifies; Ask agent sends one request to
 * one selected member.
 */

/** Envelope ids per composer scope, surviving a remount: a draft DM that
 *  lands as a real row swaps components under the same `threads:<alias>`
 *  key, and a repeat submit of the same envelope must reuse its client id
 *  (the daemon's idempotency), not mint a new message. Bounded so a long
 *  session can't grow it without limit. */
const envelopeIds = new Map<string, { key: string; id: string }>();
onPrivateSessionReset(() => envelopeIds.clear());
function envelopeId(scope: string, contentKey: string): string {
  const held = envelopeIds.get(scope);
  if (held && held.key === contentKey) return held.id;
  const id = newMessageId();
  envelopeIds.delete(scope);
  envelopeIds.set(scope, { key: contentKey, id });
  if (envelopeIds.size > 64) {
    const oldest = envelopeIds.keys().next().value;
    if (oldest !== undefined) envelopeIds.delete(oldest);
  }
  return id;
}

export default function ThreadsPage({ operator }: { operator: boolean }) {
  const href = useHref();
  const search = href.includes("?") ? href.slice(href.indexOf("?")) : "";
  const url = useMemo(() => readThreadsUrl(search), [search]);
  const draftAlias = useMemo(() => readDmParam(search), [search]);
  const humanId = useMemo(() => readHumanParam(search), [search]);
  const writeBlock = useWriteBlock(!operator);
  const t = useTranslate();
  const agentsState = useResource(resources.agents);
  const issuesState = useResource(resources.issues);
  const issueProjects = useMemo(() => issueProjectMap(issuesState.data ?? []), [issuesState.data]);

  const [list, setList] = useState<{ rows: ConversationRow[] | null; error: string | null; legacy: boolean }>({
    rows: null,
    error: null,
    legacy: false,
  });
  const [query, setQuery] = useState("");
  const [contextOpen, setContextOpen] = useState(false);
  const [createOpen, setCreateOpen] = useState(false);
  const [createMode, setCreateMode] = useState<"direct" | "group" | "project" | "agent">("direct");
  const [createBusy, setCreateBusy] = useState(false);
  const threadsTick = useSyncExternalStore(subscribeThreadsChanged, threadsRevisionNow);
  // Human discovery is the server's `me`, independent of the operator flag:
  // a verified member sees human rooms; agent conversations stay operator-only.
  const human = useHumanMe(true);
  const humanRooms = useHumanRooms(human.me !== null);
  const people = useHumanPeople(human.me !== null && humanId !== null);
  const agents = useHumanAgents(human.me !== null);
  const [humanState, setHumanState] = useState<HumanRoomState | null>(null);
  const [humanRefresh, setHumanRefresh] = useState(0);
  const onHumanState = useCallback((s: HumanRoomState) => setHumanState(s), []);
  const humanRoom = humanId ? humanRooms.rooms?.find((r) => r.id === humanId) ?? null : null;
  const humanContext = human.me && humanRoom && humanState !== null && humanState.id === humanId ? (
    <HumanContextPanel
      room={humanRoom}
      me={human.me}
      members={humanState.members}
      denied={humanState.denied}
      people={people.people}
      peopleError={people.error}
      agents={agents.agents}
      onChanged={() => {
        setHumanRefresh((n) => n + 1);
        void humanRooms.reload();
      }}
      onOpened={(id) => {
        setContextOpen(false);
        void humanRooms.reload();
        navigate(humanHref(id), { replace: true });
      }}
    />
  ) : null;

  // Only the newest list request for the current scope may publish. A slow
  // earlier request, or one started under a scope the page has since left,
  // is dropped and resolves to null.
  const listRequest = useRef(0);
  const listScope = `${operator}|${url.project ?? ""}|${url.agent ?? ""}|${url.includeSystem}`;
  const listScopeNow = useRef(listScope);
  const loadList = useCallback(async (): Promise<ConversationRow[] | null> => {
    if (!operator) return [];
    const n = ++listRequest.current;
    // Publish only the newest request; a rows result is still returned to a
    // caller in the same scope (the send that opened a draft needs it).
    const sameScope = () => listScope === listScopeNow.current;
    const latest = () => n === listRequest.current && sameScope();
    try {
      const out = await listWorkConversations({
        project: url.project ?? undefined,
        agent: url.agent ?? undefined,
        include_system: url.includeSystem,
      });
      if (latest()) setList({ rows: out.conversations, error: null, legacy: false });
      return sameScope() ? out.conversations : null;
    } catch (e) {
      if (latest()) {
        if (predatesThreads(e)) setList({ rows: [], error: null, legacy: true });
        else setList((s) => ({ ...s, error: e instanceof Error ? e.message : String(e) }));
      }
      return sameScope() ? [] : null;
    }
  }, [operator, url.project, url.agent, url.includeSystem, listScope]);

  // (1) Live: the existing SSE feed invalidates `agents` on every agent,
  // job and message change, and the `threads` frame names every topic and
  // message change (App.tsx). Reload the list on either.
  const agentsTick = agentsState.data;
  useEffect(() => {
    listScopeNow.current = listScope;
    void loadList();
  }, [loadList, agentsTick, threadsTick, listScope]);

  const rows = list.rows ?? [];
  const visible = rows
    .filter((r) => kindFilterMatches(url.kind, r.kind))
    .filter((r) => (query.trim() === "" ? true : conversationTitle(r).toLowerCase().includes(query.trim().toLowerCase())));

  const open = rows.find((r) => r.id === url.conversation) ?? null;
  // The agents on the board: the live roster the filter and chooser name. A
  // removed agent is absent from it.
  const agentNames = (agentsState.data?.agents ?? []).map((a) => a.alias).sort();
  // A draft needs a live agent, and is dropped when a real operator row for
  // that alias already exists, so the canonical row is the one opened.
  const draftLive = draftAlias !== null && agentNames.includes(draftAlias);
  const existingDm = draftAlias !== null ? rows.find((r) => r.kind === "operator" && operatorDmAlias(r) === draftAlias) ?? null : null;
  const draft = draftLive && !existingDm && !open ? draftAlias : null;
  // The user's selection from the URL: the draft alias while no conversation
  // is open, else null. A late send callback reads it, so it does not follow
  // list changes such as a newly created row.
  const currentSelection = useRef<string | null>(null);
  useEffect(() => {
    currentSelection.current = url.conversation ? null : draftAlias;
  }, [draftAlias, url.conversation]);

  const setUrl = (next: Partial<typeof url>) =>
    navigate(`/threads${threadsUrlSearch({ ...url, ...next })}`, { replace: true });

  // A `?dm=<alias>` deep link (from the Agents catalog) lands on the
  // existing You ↔ agent row when one already exists: the draft state is
  // only the not-yet-created case. The replace keeps the link copyable.
  useEffect(() => {
    if (existingDm && url.conversation === null) {
      setUrl({ conversation: existingDm.id });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [existingDm, url.conversation]);
  // Open a You ↔ agent conversation by alias: the existing row if one exists,
  // else the local draft (`?dm=`), so the page never creates an empty server row.
  const openDm = (alias: string) => {
    const row = rows.find((r) => r.kind === "operator" && operatorDmAlias(r) === alias);
    if (row) setUrl({ conversation: row.id });
    else navigate(draftHref(url, alias), { replace: true });
  };

  if (!operator && human.me === null && human.loading) {
    return (
      <section className="threads-page px-4 lg:px-8 py-5" aria-labelledby="threads-title">
        <h1 id="threads-title" className="sr-only">Threads</h1>
        <p className="threads-note" role="status">Checking your access…</p>
      </section>
    );
  }

  if (!operator && human.me === null) {
    return (
      <section className="threads-page px-4 lg:px-8 py-5" aria-labelledby="threads-title">
        <h1 id="threads-title" className="sr-only">{t("Threads")}</h1>
        <PageState title={t("Operator access required")}>
          {t("Threads is available to the operator. Use Sign in in the status bar if you have operator access.")}
        </PageState>
      </section>
    );
  }

  const context = humanId ? humanContext : open ? (
    <ContextPanel row={open} writeBlock={writeBlock} onChanged={() => void loadList()} />
  ) : null;

  return (
    <section className="threads-page" aria-labelledby="threads-title">
      <div className="threads-layout">
        <aside className="threads-list" aria-label={t("Conversations")}>
          <div className="threads-list-head">
            <h1 id="threads-title">{t("Threads")}</h1>
            <Button variant="ghost" size="sm" onClick={() => void loadList()} aria-label={t("Refresh conversations")}>
              <IconRefresh size={12} />
            </Button>
            <Button variant="secondary" size="sm" onClick={() => {
              setCreateMode(human.me ? "direct" : "agent");
              setCreateOpen(true);
            }}>
              {t("Add")}
            </Button>
          </div>
          <label className="threads-search">
            <IconSearch size={13} />
            <input
              type="search"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              placeholder={t("Search conversations")}
              aria-label={t("Search conversations")}
            />
          </label>
          {list.legacy ? (
            <p className="threads-note">{t("This daemon predates Threads. Update it to see agent conversations.")}</p>
          ) : list.error ? (
            <p className="threads-note" role="status">{t("Conversations could not load:")} {list.error}</p>
          ) : list.rows === null ? (
            // A non-operator never fetches agent conversations: no loading state for them.
            operator ? <p className="threads-note">{t("Loading conversations…")}</p> : null
          ) : visible.length === 0 ? (
            <p className="threads-note">{t("No conversations match these filters.")}</p>
          ) : (
            <ul className="threads-rows">
              {visible.map((row) => (
                <li key={row.id}>
                  <Link
                    href={`/threads${threadsUrlSearch({ ...url, conversation: row.id })}`}
                    aria-current={row.id === open?.id ? "true" : undefined}
                    className={row.id === open?.id ? "threads-row on" : "threads-row"}
                  >
                    <span className="threads-row-top">
                      <span className="threads-row-title">{conversationTitle(row, t)}</span>
                      <span className="threads-row-kind">{t(KIND_LABEL[row.kind])}</span>
                    </span>
                    <span className="threads-row-meta">{rowDescriptor(row, t)}</span>
                  </Link>
                </li>
              ))}
            </ul>
          )}
          {human.error !== null && <p className="threads-note">Human rooms are unavailable for this sign-in.</p>}
          {human.me && url.kind === "all" && humanRooms.rooms !== null && humanRooms.rooms.length > 0 && (
            <ul className="threads-rows" aria-label="Human rooms">
              <HumanRoomRows
                rooms={humanRooms.rooms.filter((r) => query.trim() === "" || r.title.toLowerCase().includes(query.trim().toLowerCase()))}
                openId={humanId}
                hrefFor={humanHref}
              />
            </ul>
          )}
        </aside>

        <main className="threads-main" aria-label={t("Conversation")}>
          {humanId ? (
            human.me ? (
              <HumanRoomTimeline
                key={humanId}
                id={humanId}
                me={human.me}
                refresh={humanRefresh}
                onState={onHumanState}
                onOpenContext={() => setContextOpen(true)}
                onChanged={humanRooms.reload}
                isDm={humanRoom?.kind === "dm"}
              />
            ) : (
              <div className="threads-empty">
                <p>Human rooms are unavailable for this sign-in.</p>
              </div>
            )
          ) : open ? (
            <Timeline
              key={open.id}
              row={open}
              scope={{ project: url.project ?? undefined, agent: url.agent ?? undefined, include_system: url.includeSystem }}
              writeBlock={writeBlock}
              liveStamp={agentsTick}
              threadsTick={threadsTick}
              issueProjects={issueProjects}
              agents={agentsState.data?.agents ?? null}
              onOpenContext={() => setContextOpen(true)}
              onChanged={() => void loadList()}
              openDm={openDm}
            />
          ) : draft ? (
            <DraftDm
              key={draft}
              alias={draft}
              writeBlock={writeBlock}
              threadsTick={threadsTick}
              liveStamp={agentsTick}
              issueProjects={issueProjects}
              agents={agentsState.data?.agents ?? null}
              onSent={() => {
                // The send created the thread: open its canonical row once listed.
                void loadList().then((fresh) => {
                  // Only if the user is still on this draft: a late reply must not
                  // pull them off a conversation or draft they chose since.
                  if (currentSelection.current !== draft) return;
                  const row = fresh?.find((r) => r.kind === "operator" && operatorDmAlias(r) === draft);
                  if (row) setUrl({ conversation: row.id });
                });
              }}
            />
          ) : (
            <div className="threads-empty">
              <span aria-hidden><IconThreads size={28} /></span>
              {draftAlias !== null ? (
                <p>
                  {agentsState.data === null
                    ? t("Checking the agent roster…")
                    : t("No registered agent named {alias} — the draft link may be stale.")
                        .replace("{alias}", () => draftAlias)}
                </p>
              ) : (
                <p>{t("Select a conversation to read it.")}</p>
              )}
            </div>
          )}
        </main>

        {/* The one roster: the rail on desktop. Below the desktop width the
            rail is hidden and Context opens the same panel. */}
        <aside className="threads-context" aria-label={t("Participants")}>
          {context}
        </aside>
      </div>
      {(open || humanId) && (
        <SheetDialog open={contextOpen} title="Context" className="threads-context-sheet" onClose={() => setContextOpen(false)}>
          {context}
        </SheetDialog>
      )}
      <SheetDialog open={createOpen} title="New conversation" className="threads-create-sheet"
        busy={createBusy} onClose={() => setCreateOpen(false)}>
        {createOpen && <>
          <div className="threads-create-modes" aria-label="Conversation type">
            {([
              ...(human.me ? [{ value: "direct" as const, label: "Direct" }, { value: "group" as const, label: "Group" }] : []),
              ...(operator || createMode === "project" || createMode === "agent" ? [{ value: "project" as const, label: "Project" }, { value: "agent" as const, label: "Agent" }] : []),
            ]).map((mode) => <Button key={mode.value} variant="ghost"
              disabled={createBusy || (!operator && (mode.value === "project" || mode.value === "agent"))}
              aria-pressed={createMode === mode.value} onClick={() => setCreateMode(mode.value)}>{t(mode.label)}</Button>)}
          </div>
          {human.me && (createMode === "direct" || createMode === "group") && (
            <NewHumanConversation key={createMode} mode={createMode} me={human.me}
              agents={agents.agents} agentsError={agents.error} onBusy={setCreateBusy}
              onCancel={() => setCreateOpen(false)} onCreated={(id) => {
                setCreateOpen(false);
                void humanRooms.reload();
                navigate(humanHref(id), { replace: true });
              }} />
          )}
          {createMode === "project" && <NewGroup writeBlock={writeBlock}
            onCancel={() => setCreateOpen(false)} onBusy={setCreateBusy} onCreated={(id) => {
              setCreateOpen(false);
              void loadList();
              setUrl({ conversation: id });
            }} />}
          {createMode === "agent" && <NewThread agents={agentNames} current={draft}
            writeBlock={writeBlock} onCancel={() => setCreateOpen(false)} onOpen={(alias) => {
              setCreateOpen(false);
              openDm(alias);
            }} />}
        </>}
      </SheetDialog>
    </section>
  );
}

/** `?h=<room>`: an open human room (CAD-1412). */
function readHumanParam(search: string): string | null {
  const v = new URLSearchParams(search).get("h");
  return v === null || v.trim() === "" ? null : v;
}

function humanHref(id: string): string {
  return `/threads?h=${encodeURIComponent(id)}`;
}

/** `?dm=<alias>`: a local draft You ↔ agent conversation (no server row yet). */
function readDmParam(search: string): string | null {
  const v = new URLSearchParams(search).get("dm");
  return v === null || v.trim() === "" ? null : v;
}

/** A draft's URL: the page's list state plus `dm=<alias>`. */
function draftHref(state: Partial<ThreadsUrlState>, alias: string): string {
  const base = threadsUrlSearch({ ...state, conversation: null });
  return `/threads${base ? `${base}&` : "?"}dm=${encodeURIComponent(alias)}`;
}

const KIND_LABEL: Record<ConversationRow["kind"], string> = {
  pair: "Agents",
  operator: "You ↔ agent",
  group: "Group",
  unknown: "Unknown sender",
};

/** A native modal sheet (the repo's dialog pattern), closed by Escape or its button. Children stay mounted while closed, so form values survive a close and reopen. */
function SheetDialog({
  open,
  title,
  className,
  onClose,
  children,
  busy = false,
}: {
  open: boolean;
  title: string;
  className: string;
  onClose: () => void;
  children: ReactNode;
  busy?: boolean;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  const t = useTranslate();
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    if (open && !dialog.open) dialog.showModal();
    if (!open && dialog.open) dialog.close();
  }, [open]);
  return (
    <dialog
      ref={ref}
      className={className}
      aria-labelledby={titleId}
      onCancel={(event) => {
        event.preventDefault();
        if (busy) return;
        onClose();
      }}
    >
      <div className="threads-context-sheet-head">
        <h2 id={titleId}>{t(title)}</h2>
        <Button variant="ghost" size="sm" disabled={busy} onClick={onClose} aria-label={t(`Close ${title.toLowerCase()}`)} icon={<IconClose />} />
      </div>
      {children}
    </dialog>
  );
}

/** Issue id -> project key, from the board's issue cards (the app's join). */
type IssueProjects = Map<string, string>;
function issueProjectMap(issues: { id: string; project: string }[]): IssueProjects {
  return new Map(issues.map((i) => [i.id, i.project]));
}


/** A row's second line from real data only: the last preview; else a group's
 *  post and member counts; else a direct thread's label. */
function rowDescriptor(row: ConversationRow, t: (english: string) => string = (english) => english): string {
  if (row.last) return row.last.preview;
  if (row.kind === "group") {
    const members = liveMembers(row).length;
    const parts = [`${members} ${t(members === 1 ? "member" : "members")}`];
    if (row.posts !== undefined) parts.unshift(`${row.posts} ${t(row.posts === 1 ? "post" : "posts")}`);
    return parts.join(" · ");
  }
  if (row.kind === "operator") return t("Direct thread");
  return t("No messages");
}

/** The initial shown in a participant's identity circle. */
function initialOf(label: string): string {
  return label.trim().charAt(0).toUpperCase() || "?";
}

function Timeline({
  row,
  scope,
  writeBlock,
  liveStamp,
  threadsTick,
  issueProjects,
  agents,
  onOpenContext,
  onChanged,
  openDm,
}: {
  row: ConversationRow;
  /** The list's filter context: the timeline reads the same scope. */
  scope: ConversationListFilter;
  writeBlock: string | null;
  issueProjects: IssueProjects;
  /** The live roster (null while it loads): the DM's recipient context
   *  strip reads it; a missing alias after load is honestly absent. */
  agents: Agent[] | null;
  /** The live `agents` payload: a new object on every stream frame. */
  liveStamp: unknown;
  /** The `threads` frame revision: a topic or message change on any thread. */
  threadsTick: number;
  openDm: (alias: string) => void;
  onOpenContext: () => void;
  onChanged: () => void;
}) {
  const isGroup = row.kind === "group";
  const dm = operatorDmAlias(row);
  const t = useTranslate();
  const [messages, setMessages] = useState<ThreadMessage[] | null>(null);
  const [entries, setEntries] = useState<ThreadEntry[] | null>(null);
  const [posts, setPosts] = useState<TopicPost[] | null>(null);
  const [before, setBefore] = useState<number | null>(null);
  const [more, setMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  const scopeKey = `${scope.project ?? ""}|${scope.agent ?? ""}|${scope.include_system ? 1 : 0}`;

  /** One read of this conversation in its kind's shape, ascending by seq. */
  const pull = async (at: ReadPageOptions): Promise<Pulled> => {
    if (dm) {
      // Operator DM: the agent's home thread, read by its conversation id.
      const p = await api.thread(dm, { ...at, limit: PAGE, conversation: row.id });
      return {
        entries: threadEntries(p.entries),
        cursor: p.cursor ?? at.after ?? 0,
        more_before: p.more_before === true,
        // Raw row count: a malformed row dropped by threadEntries is still a row the daemon sent.
        full: p.entries.length >= PAGE,
      };
    }
    if (isGroup) {
      const p = await readTopic(row.id, { ...at, limit: PAGE });
      return { posts: p.posts, cursor: p.cursor, more_before: p.more_before, full: p.posts.length >= PAGE };
    }
    const p = await readWorkConversation(row.id, { ...at, limit: PAGE }, scope);
    // The daemon filters system rows after computing `cursor`, so the raw
    // page length is unknown here: a forward read continues while the cursor advances.
    return {
      messages: p.messages,
      cursor: p.cursor,
      more_before: p.more_before,
      full: p.cursor > (at.after ?? 0),
    };
  };
  const absorb = ({ entries: e, posts: po, messages: m }: Pulled) => {
    if (e) setEntries((cur) => mergeBySeq(cur, e));
    if (po) setPosts((cur) => mergeBySeq(cur, po));
    if (m) setMessages((cur) => mergeBySeq(cur, m));
  };

  // Epoch: one conversation and list scope. A change resets the timeline and
  // drops every read of the old epoch. `live` numbers this view's own reads:
  // only the newest one publishes, so two overlapping tail reads never merge.
  // `loadEarlier` is gated by the epoch only, so a live tick does not drop it.
  const epoch = useRef({ id: "", n: 0 });
  const live = useRef(0);
  const identity = `${row.id}|${scopeKey}`;
  useEffect(() => {
    const reset = epoch.current.id !== identity;
    if (reset) {
      epoch.current = { id: identity, n: epoch.current.n + 1 };
      setEntries(null);
      setPosts(null);
      setMessages(null);
      setBefore(null);
      setMore(false);
    }
    const n = epoch.current.n;
    const g = ++live.current;
    const current = () => g === live.current && epoch.current.n === n;
    const loaded = reset ? null : dm ? entries : isGroup ? posts : messages;
    // Pages are ascending, so the window's first row is its oldest seq.
    const oldest = loaded && loaded.length > 0 ? loaded[0].seq : null;
    const fail = (e: unknown) => {
      if (current()) setError(e instanceof ApiError ? e.message : String(e));
    };
    if (oldest === null) {
      pull({ tail: true })
        .then((p) => {
          if (!current()) return;
          absorb(p);
          setBefore(oldestOf(p));
          setMore(p.more_before);
          setError(null);
        })
        .catch(fail);
      return;
    }
    // Refresh the loaded window: forward from just below its oldest row (the
    // `after` cursor is exclusive), to the current end. Rows already held are
    // replaced by seq, so a queued row's later state is seen. `before` and
    // `more_before` are left alone: they belong to earlier history.
    const step = async (after: number): Promise<void> => {
      const p = await pull({ after });
      if (!current()) return;
      absorb(p);
      setError(null);
      if (p.full && p.cursor > after) await step(p.cursor);
    };
    step(oldest - 1).catch(fail);
  }, [identity, liveStamp, threadsTick, tick]);

  // (5) Earlier history: page backwards from the oldest loaded seq; a short
  // page means the start is reached. Nothing is dropped silently. A result
  // from an old epoch is discarded.
  const loadEarlier = async () => {
    if (before === null) return;
    const n = epoch.current.n;
    try {
      const p = await pull({ before });
      if (epoch.current.n !== n) return;
      absorb(p);
      setBefore(oldestOf(p) ?? before);
      setMore(p.more_before);
    } catch (e) {
      if (epoch.current.n === n) setError(e instanceof ApiError ? e.message : String(e));
    }
  };

  const reload = () => {
    setTick((t) => t + 1);
    onChanged();
  };

  return (
    <div className="threads-timeline">
      <header className="threads-head">
        <div className="threads-kicker">{t(KIND_LABEL[row.kind])}</div>
        <h2>{conversationTitle(row, t)}</h2>
        <div className="threads-head-actions">
          <Button variant="ghost" size="sm" onClick={onOpenContext} className="threads-context-toggle">
            {t("Context")}
          </Button>
        </div>
      </header>
      {dm && <DmContext alias={dm} agents={agents} issueProjects={issueProjects} />}
      <div className="threads-messages" aria-live="polite">
        {error ? (
          <p className="threads-note" role="status">{t("This conversation could not load:")} {error}</p>
        ) : (dm ? entries : isGroup ? posts : messages) === null ? (
          <p className="threads-note">{t("Loading…")}</p>
        ) : (
          <>
            {more && (
              <Button variant="ghost" size="sm" onClick={() => void loadEarlier()}>{t("Load earlier")}</Button>
            )}
            {dm
              ? (entries ?? []).map((e) => <EntryRow key={e.seq} entry={e} issueProjects={issueProjects} />)
              : isGroup
                ? (posts ?? []).map((p) => <PostRow key={p.id} post={p} />)
                : (messages ?? []).map((m) => <MessageRow key={m.id} message={m} issueProjects={issueProjects} />)}
            {isGroup
              ? (posts ?? []).length === 0 && <p className="threads-note">{t("No posts yet.")}</p>
              : (dm ? entries ?? [] : messages ?? []).length === 0 && <p className="threads-note">{t("No messages yet.")}</p>}
          </>
        )}
      </div>
      {row.kind === "operator" && dm && (
        <OperatorComposer
          alias={dm}
          agent={agents?.find((a) => a.alias === dm)}
          writeBlock={writeBlock}
          onSent={reload}
        />
      )}
      {row.kind === "pair" && <ReplyBar row={row} openDm={openDm} />}
      {isGroup && (
        <GroupComposer
          row={row}
          writeBlock={writeBlock}
          onSent={reload}
        />
      )}
    </div>
  );
}

/** Parse a thread page's entries, dropping any the Home model rejects. */
function threadEntries(raw: unknown[]): ThreadEntry[] {
  return raw.map(asEntry).filter((e): e is ThreadEntry => e !== null);
}

/** Rows per read. */
const PAGE = 50;

/** One read: the rows of this conversation's kind, and its cursors. */
type Pulled = {
  entries?: ThreadEntry[];
  posts?: TopicPost[];
  messages?: ThreadMessage[];
  cursor: number;
  more_before: boolean;
  /** A forward read may have rows past `cursor`. */
  full: boolean;
};

/** The oldest row of a page (pages are ascending): the next `before` cursor. */
function oldestOf(p: Pulled): number | null {
  return p.entries?.[0]?.seq ?? p.posts?.[0]?.seq ?? p.messages?.[0]?.seq ?? null;
}

/** Merge rows by seq: dedupe, keep seq order, never drop a loaded row. */
function mergeBySeq<T extends { seq: number }>(cur: T[] | null, incoming: T[]): T[] {
  const bySeq = new Map<number, T>();
  for (const r of [...(cur ?? []), ...incoming]) bySeq.set(r.seq, r);
  return [...bySeq.values()].sort((a, b) => a.seq - b.seq);
}

/** Stages the daemon's `payload.delivery` may carry and the page shows. */
const STAGES = new Set<string>(["queued", "claimed", "available", "running", "completed", "failed", "unknown", "refused", "interrupted"]);

/** An operator DM entry from the agent's thread. Its stage is the daemon's
 *  `payload.delivery.state`, present only for a message-linked entry; an
 *  unlinked entry or an unknown state shows no stage. `payload.refs` are
 *  the cited subjects the send carried, and a verified `payload.human`
 *  the recorded owner behind an owner write — both shown, never inferred. */
function EntryRow({ entry, issueProjects }: { entry: ThreadEntry; issueProjects: IssueProjects }) {
  const t = useTranslate();
  const delivery = entry.payload?.delivery as { state?: unknown } | undefined;
  const stage = typeof delivery?.state === "string" && STAGES.has(delivery.state) ? delivery.state : null;
  const refs = Array.isArray(entry.payload?.refs)
    ? (entry.payload.refs as { kind?: unknown; id?: unknown }[]).filter(
        (r): r is { kind: string; id: string } => typeof r?.kind === "string" && typeof r?.id === "string",
      )
    : [];
  const human = entry.payload?.human as { display?: unknown } | null | undefined;
  const senderName =
    entry.role === "operator" && typeof human?.display === "string" && human.display.trim() !== ""
      ? human.display
      : null;
  const who = entry.role === "operator"
    ? senderName ?? t("Operator (name unknown)")
    : entry.role === "agent" ? t("Agent") : t("System");
  return (
    <article className="threads-msg" data-role={entry.role}>
      <Turn who={who} at={entry.created ? fmtTime(entry.created) : undefined} me={entry.role === "operator"} state={stage}>
        {entry.text}
      </Turn>
      {refs.length > 0 && (
        <div className="threads-refs">
          {refs.map((r) => {
            const project = r.kind === "issue" ? issueProjects.get(r.id) : undefined;
            return project ? (
              <Link key={`${r.kind}:${r.id}`} href={issuePath(project, r.id)} className="threads-ref">{r.id}</Link>
            ) : (
              <span key={`${r.kind}:${r.id}`} className="threads-ref">
                {r.kind === "issue" ? r.id : `${r.kind}:${r.id}`}
              </span>
            );
          })}
        </div>
      )}
    </article>
  );
}

function MessageRow({ message, issueProjects }: { message: ThreadMessage; issueProjects: IssueProjects }) {
  const t = useTranslate();
  const issueProject = message.issue ? issueProjects.get(message.issue) : undefined;
  const routed = (message.routed ?? []).filter((r) => typeof r.id === "string");
  const recipient =
    message.recipient && message.recipient.kind !== "unknown"
      ? participantLabel(message.recipient, t)
      : null;
  return (
    <article className="threads-msg" data-role={message.sender.kind} data-state={message.state}>
      <Turn
        who={participantLabel(message.sender, t)}
        at={fmtTime(message.created)}
        me={message.sender.kind === "operator"}
        state={message.state}
      >
        {message.body}
      </Turn>
      {(message.issue || message.task_id || message.origin || routed.length > 0 || recipient) && (
        <div className="threads-refs">
          {recipient && (
            <span className="threads-ref">
              {t("To {name}").replace("{name}", () => recipient)}
            </span>
          )}
          {message.origin && (
            <span className="threads-ref">
              {t("Reply to message {id}").replace("{id}", () => message.origin as string)}
            </span>
          )}
          {message.issue && (issueProject ? (
            <Link href={issuePath(issueProject, message.issue)} className="threads-ref">{message.issue}</Link>
          ) : (
            <span className="threads-ref">{message.issue}</span>
          ))}
          {message.task_id && <span className="threads-ref">{t("task {task}").replace("{task}", () => String(message.task_id))}</span>}
          {routed.map((r) => (
            <span key={r.id} className="threads-ref">
              {t("Reply routed · {source}").replace("{source}", () => t(SOURCE_LABEL[r.source] ?? r.source))}
              {" — "}
              <StageLabel state={receiptState(r.state)} />
            </span>
          ))}
        </div>
      )}
      {message.error && <p className="threads-msg-error" role="status">{message.error}</p>}
    </article>
  );
}

/** A routed copy's `source` as a readable label; unknown values stay verbatim. */
const SOURCE_LABEL: Record<string, string> = {
  worker_result: "worker result",
  worker_notice: "worker notice",
  job_event: "job event",
  nudge: "nudge",
  topic_notice: "topic notice",
  kickoff: "kickoff",
  job_dispatch: "job dispatch",
};

/** Every delivery state, in display order: attention states first. */
const RECEIPT_ORDER: DeliveryState[] = ["failed", "refused", "interrupted", "unknown", "pending", "queued", "claimed", "running", "available", "completed"];

/** A receipt's state for display: an unsupported string shows as Unknown, never a guessed stage. */
function receiptState(state: string): DeliveryState {
  return (RECEIPT_ORDER as string[]).includes(state) ? (state as DeliveryState) : "unknown";
}

/** A topic post: its transcript body and each recipient's real receipt. */
function PostRow({ post }: { post: TopicPost }) {
  const t = useTranslate();
  const operator = post.sender.alias === "operator";
  return (
    <article className="threads-msg" data-role={operator ? "operator" : "agent"}>
      <Turn who={operator ? t("You") : post.sender.alias} at={fmtTime(post.created)} me={operator}>
        {post.body}
      </Turn>
      {post.deliveries.length > 0 && <DeliveryFeedback deliveries={post.deliveries} />}
    </article>
  );
}

/** Delivery feedback from the post's real receipts only. One receipt names its
 *  recipient and state. Several show a count summary by actual state, which
 *  expands natively into each recipient's own row. */
function DeliveryFeedback({ deliveries }: { deliveries: PostDelivery[] }) {
  const t = useTranslate();
  const counts = new Map<DeliveryState, number>();
  for (const d of deliveries) {
    const s = receiptState(d.state);
    counts.set(s, (counts.get(s) ?? 0) + 1);
  }
  const states = RECEIPT_ORDER.filter((s) => counts.has(s));
  const name = (d: PostDelivery) => (typeof d.alias === "string" && d.alias.trim() !== "" ? d.alias : t("Unknown recipient"));
  const removed = (d: PostDelivery) => (d.removed ? ` ${t("(removed)")}` : "");

  if (deliveries.length === 1) {
    const d = deliveries[0];
    return (
      <div className="threads-receipts" aria-label={t("Delivery to members")}>
        <span className="threads-receipt-name">{`${name(d)}${removed(d)} · `}</span>
        <StageLabel state={receiptState(d.state)} />
      </div>
    );
  }

  return (
    <div className="threads-receipts" aria-label={t("Delivery to members")}>
      <details className="threads-receipt-details">
        <summary>
          {states.map((s, i) => (
            <span key={s}>
              {i > 0 && " · "}
              <StageLabel state={s} /> {counts.get(s)}
            </span>
          ))}
        </summary>
        {states.includes("available") && (
          <p className="threads-note threads-receipt-note">{t("Available in the recipient’s inbox; not read or seen.")}</p>
        )}
        <ul className="threads-receipt-list">
          {deliveries.map((d) => (
            <li key={`${d.uid}:${d.message ?? ""}`} data-removed={d.removed || undefined}>
              <span className="threads-receipt-name">{`${name(d)}${removed(d)}`}</span>
              <StageLabel state={receiptState(d.state)} />
            </li>
          ))}
        </ul>
      </details>
    </div>
  );
}

function OperatorComposer({
  alias,
  agent,
  writeBlock,
  onSent,
}: {
  alias: string;
  /** The recipient's roster row when it is live, else undefined. */
  agent?: Agent;
  writeBlock: string | null;
  onSent: () => void;
}) {
  // One client id per logical envelope: an exact retry after a failed
  // attempt reuses it (one DM, not two); any change to the submitted
  // body, refs or attachments mints a new id. The id lives in a module
  // map, so a draft→row remount of this composer keeps the same id.
  const t = useTranslate();
  const [notice, setNotice] = useState<UiNotice | null>(null);
  const scope = `threads:${alias}`;
  return (
    <ThreadComposerFrame
      showModes={false}
      allowAsk={false}
      helper={t(dmExpectation(agent))}
      notice={notice ? displayUiNotice(notice, t) : null}
    >
      <Composer
        density="full"
        block={writeBlock}
        storeKey={scope}
        onSend={async ({ body, refs, attachments }) => {
          const key = JSON.stringify({ alias, body: body.trim(), refs, attachments: attachments ?? [] });
          const id = envelopeId(scope, key);
          try {
            await withPrivateOperation(async (op) => {
              await api.threadSend(alias, body, id, refs, undefined, undefined, attachments, op);
              op.assertCurrent();
              envelopeIds.delete(scope);
              onSent();
            });
            setNotice(null);
          } catch (e) {
            // A lost response cannot establish whether the server accepted it.
            setNotice({
              key: e instanceof ApiError && e.status >= 400 && e.status < 500
                ? "Send refused. Your draft is kept. "
                : "Delivery uncertain — check history before retrying. Your draft is kept. ",
              detail: e instanceof Error ? e.message : String(e),
            });
            throw e;
          }
        }}
        onNotice={setNotice}
        refocusAfterSend
        attach
        ariaLabel={t("Message to {alias}").replace("{alias}", () => alias)}
        placeholder={t("Message {alias}…").replace("{alias}", () => alias)}
        className="card p-2.5 relative"
      />
    </ThreadComposerFrame>
  );
}


/** The DM's recipient context strip: the live roster's avatar, lifecycle
 *  badge and first active task as the recorded work context. An agent
 *  absent from a loaded roster — removed or otherwise unregistered —
 *  says so; while the roster is still loading nothing is claimed. */
function DmContext({
  alias,
  agents,
  issueProjects,
}: {
  alias: string;
  /** The live roster; null while it loads. */
  agents: Agent[] | null;
  issueProjects: IssueProjects;
}) {
  const t = useTranslate();
  const agent = agents?.find((a) => a.alias === alias);
  const badge = agent ? lifecycleBadge(agent, t) : null;
  const task = agent ? activeTasks(agent)[0] : undefined;
  const issueProject = task?.issue ? issueProjects.get(task.issue) : undefined;
  return (
    <div className="threads-recipient">
      {agent ? (
        <AgentAvatar
          slug={lookFor(agent.alias)}
          active={lifecycleOf(agent) === "active"}
          still={lifecycleOf(agent) !== "active"}
          size={26}
        />
      ) : (
        <span aria-hidden className="threads-avatar">{initialOf(alias)}</span>
      )}
      <span className="threads-recipient-line">
        <strong>{t("To {alias}").replace("{alias}", () => alias)}</strong>
        {badge && (
          <span className={`stateb tone-${badge.tone}`}>
            <i aria-hidden="true" />
            {badge.label}
          </span>
        )}
        {!agent && (
          <span className="threads-ref">
            {agents !== null
              ? t("Not in the live roster — removed or unavailable")
              : t("Loading roster…")}
          </span>
        )}
      </span>
      <span className="threads-recipient-work">
        {task ? (
          <>
            {task.issue && issueProject ? (
              <Link href={issuePath(issueProject, task.issue)} className="threads-ref">{task.issue}</Link>
            ) : task.issue ? (
              <span className="threads-ref">{task.issue}</span>
            ) : null}
            <span className="threads-ref">{task.title ?? task.task}</span>
          </>
        ) : agent ? (
          <span className="threads-ref">{t("No recorded work context")}</span>
        ) : null}
      </span>
    </div>
  );
}

/** Agent-to-agent conversations have no composer. Reply opens a direct message
 *  to one chosen participant, inside Threads. */
function ReplyBar({ row, openDm }: { row: ConversationRow; openDm: (alias: string) => void }) {
  const t = useTranslate();
  const agents = row.participants.filter((p) => p.kind === "agent" && !p.removed && p.alias);
  return (
    <div className="threads-reply">
      <span>{t("Agent conversation · read only.")}</span>
      {agents.map((p) => (
        <Button key={p.uid ?? p.alias ?? "agent"} variant="secondary" size="sm" onClick={() => openDm(p.alias as string)}>
          {t("Reply to {alias}").replace("{alias}", () => p.alias as string)}
        </Button>
      ))}
    </div>
  );
}

/** New thread: choose a live agent and open its You ↔ agent conversation. */
function NewThread({ agents, current, onOpen, onCancel, writeBlock }: {
  agents: string[];
  current: string | null;
  onOpen: (alias: string) => void;
  onCancel: () => void;
  writeBlock: string | null;
}) {
  const [alias, setAlias] = useState("");
  const id = useId();
  const t = useTranslate();
  return (
    <form className="threads-create-form" aria-label={t("New thread")} onSubmit={(event) => {
      event.preventDefault();
      if (writeBlock === null && agents.includes(alias)) onOpen(alias);
    }}>
      <div className="threads-create-body">
        <p className="threads-note">{t("Start a focused thread with a current agent.")}</p>
        <div className="threads-form-field">
          <label className="threads-field-label" htmlFor={id}>{t("Agent for new thread")}</label>
          <Select id={id} full searchable value={alias} onChange={setAlias}
            options={agents.map((agent) => ({ value: agent, label: agent }))}
            placeholder={agents.length === 0 ? t("No live agents") : t("Choose an agent")}
            disabled={writeBlock !== null} />
        </div>
        {current && <p className="threads-note">{t("Draft with {alias}").replace("{alias}", () => current)}</p>}
        {writeBlock && <p className="threads-note" role="status">{writeBlock}</p>}
      </div>
      <div className="threads-create-footer"><span className="threads-note">{t("No message is sent until you write one.")}</span>
        <div><Button variant="secondary" onClick={onCancel}>{t("Cancel")}</Button>
          <Button variant="primary" type="submit" disabled={writeBlock !== null || !agents.includes(alias)}>{t("Start thread")}</Button></div>
      </div>
    </form>
  );
}

/** A local draft You ↔ agent conversation. Nothing exists on the server until
 *  the first valid send; the empty thread read creates nothing. The send
 *  creates the thread, and the list then offers its canonical row. */
function DraftDm({
  alias,
  writeBlock,
  threadsTick,
  liveStamp,
  issueProjects,
  agents,
  onSent,
}: {
  alias: string;
  writeBlock: string | null;
  threadsTick: number;
  liveStamp: unknown;
  issueProjects: IssueProjects;
  /** The live roster; null while it loads. */
  agents: Agent[] | null;
  onSent: () => void;
}) {
  const [entries, setEntries] = useState<ThreadEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const t = useTranslate();
  useEffect(() => {
    let live = true;
    // No `conversation` selector: the home read of an alias with no thread is empty.
    api
      .thread(alias, { tail: true, limit: 50 })
      .then((p) => {
        if (!live) return;
        setError(null);
        setEntries(threadEntries(p.entries));
      })
      .catch((e: unknown) => {
        if (live) setError(e instanceof ApiError ? e.message : String(e));
      });
    return () => {
      live = false;
    };
  }, [alias, threadsTick, liveStamp]);
  return (
    <div className="threads-timeline">
      <header className="threads-head">
        <div className="threads-kicker">{t(KIND_LABEL.operator)}</div>
        <h2>{t("You ↔ {alias}").replace("{alias}", () => alias)}</h2>
      </header>
      <DmContext alias={alias} agents={agents} issueProjects={issueProjects} />
      <div className="threads-messages" aria-live="polite">
        {error ? (
          <p className="threads-note" role="status">This conversation could not load: {error}</p>
        ) : entries === null ? (
          <p className="threads-note">Loading…</p>
        ) : (
          <>
            {entries.map((e) => <EntryRow key={e.seq} entry={e} issueProjects={issueProjects} />)}
            {entries.length === 0 && <p className="threads-note">{t("No messages yet. Sending starts the thread.")}</p>}
          </>
        )}
      </div>
      <OperatorComposer
        alias={alias}
        agent={agents?.find((a) => a.alias === alias)}
        writeBlock={writeBlock}
        onSent={onSent}
      />
    </div>
  );
}

/** Live members of a group: current subscribers, not removed. */
function liveMembers(row: ConversationRow): Participant[] {
  return row.participants.filter((p) => p.kind === "agent" && !p.removed && p.alias);
}

/** Logical-envelope identity for a group send: the group composer forwards
 *  no refs or attachments, so the key is the mode, the target and the body.
 *  Same key → same client id on retry; a change mints a new id. */
function groupEnvelopeKey(mode: "post" | "ask", target: string, body: string): string {
  return JSON.stringify({ mode, target: mode === "ask" ? target : "", body: body.trim() });
}

/** Group posts notify; Ask agent sends one request to one selected member. */
function GroupComposer({
  row,
  writeBlock,
  onSent,
}: {
  row: ConversationRow;
  writeBlock: string | null;
  onSent: () => void;
}) {
  const [mode, setMode] = useState<"post" | "ask">("post");
  const members = liveMembers(row);
  const [target, setTarget] = useState("");
  const t = useTranslate();
  const [notice, setNotice] = useState<UiNotice | null>(null);
  const noMembers = members.length === 0;
  const askBlocked = mode === "ask" && (noMembers || !members.some((m) => m.alias === target));

  return (
    <ThreadComposerFrame
      modeLabel={t("Group message type")}
      allowAsk
      mode={mode}
      onMode={setMode}
      targetLabel={t("Member to ask")}
      target={target}
      onTarget={setTarget}
      targetOptions={members.map((m) => ({ id: m.alias as string, label: m.alias as string }))}
      targetPlaceholder={noMembers ? t("No current members") : t("Choose a member")}
      helper={mode === "post"
        ? t("A post notifies members. It does not start agents.")
        : t("One request, to the member you choose. Nothing else is sent.")}
      notice={notice ? displayUiNotice(notice, t) : null}
    >
      <Composer
        density="full"
        block={writeBlock !== null ? writeBlock : askBlocked ? "Choose a current member to ask" : null}
        storeKey={`threads-group:${row.id}`}
        onSend={async ({ body }) => {
          const scope = `threads-group:${row.id}`;
          const id = envelopeId(scope, groupEnvelopeKey(mode, target, body));
          try {
            if (mode === "ask") {
              await requestFromTopic(row.id, target, body, id);
            } else {
              await postToTopic(row.id, body, id);
            }
            envelopeIds.delete(scope);
            setNotice(null);
            onSent();
          } catch (e) {
            // The draft stays for an explicit same-id retry; say why it failed.
            setNotice(e instanceof Error ? e.message : String(e));
            throw e;
          }
        }}
        onNotice={setNotice}
        refocusAfterSend
        ariaLabel={mode === "post" ? t("Post to group") : t("Ask one member")}
        placeholder={mode === "post" ? t("Post to the group…") : t("Ask one member…")}
        className="card p-2.5 relative"
      />
    </ThreadComposerFrame>
  );
}

/** The participants rail / Context body: the single roster. */
function ContextPanel({
  row,
  writeBlock,
  onChanged,
}: {
  row: ConversationRow;
  writeBlock: string | null;
  onChanged: () => void;
}) {
  const t = useTranslate();
  const [error, setError] = useState<string | null>(null);
  const [alias, setAlias] = useState("");
  const isGroup = row.kind === "group";
  const act = async (fn: () => Promise<unknown>) => {
    setError(null);
    try {
      await fn();
      onChanged();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e));
    }
  };
  return (
    <div className="threads-context-body">
      <div className="threads-context-heading">
        <h3 className="threads-context-title">{t("Participants")}</h3>
        <span className="threads-context-count">{row.participants.filter((p) => !p.removed).length}</span>
      </div>
      <ul className="threads-roster">
        {row.participants.map((p) => (
          <li key={p.uid ?? p.alias ?? p.kind} data-kind={p.kind} data-removed={p.removed || undefined}>
            <span className="threads-person">
              <span aria-hidden className="threads-avatar">{initialOf(participantLabel(p, t))}</span>
              <span>{participantLabel(p, t)}</span>
            </span>
            {isGroup && p.kind === "agent" && p.alias && !p.removed && (
              <Button
                variant="ghost"
                size="sm"
                disabled={writeBlock !== null}
                onClick={() => void act(() => changeSubscriber(row.id, "unsubscribe", p.alias as string))}
              >
                {t("Remove")}
              </Button>
            )}
          </li>
        ))}
      </ul>
      {isGroup && (
        <form
          className="threads-add"
          onSubmit={(e) => {
            e.preventDefault();
            const value = alias.trim();
            if (!value) return;
            void act(async () => {
              await changeSubscriber(row.id, "subscribe", value);
              setAlias("");
            });
          }}
        >
          <label>
            <span className="threads-field-label">{t("Add an agent by alias")}</span>
            <input value={alias} onChange={(e) => setAlias(e.target.value)} placeholder={t("Add agent alias")} />
          </label>
          <Button variant="secondary" size="sm" type="submit" disabled={writeBlock !== null || alias.trim() === ""}>{t("Add")}</Button>
        </form>
      )}
      {error && <p className="threads-note" role="status">{error}</p>}
      <p className="threads-note">{t("Workspace access applies. Linking work does not grant access.")}</p>
    </div>
  );
}

/** Operator creates a `fleet:` or `project:` topic (the CAD-810 allowlist). */
function NewGroup({ writeBlock, onCreated, onCancel, onBusy }: {
  writeBlock: string | null;
  onCreated: (id: string) => void;
  onCancel: () => void;
  onBusy: (busy: boolean) => void;
}) {
  const t = useTranslate();
  const id = useId();
  const [type, setType] = useState<"fleet" | "project">("project");
  const [source, setSource] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <form className="threads-create-form" onSubmit={(event) => {
      event.preventDefault();
      if (pending || writeBlock !== null || source.trim() === "") return;
      setError(null);
      setPending(true);
      onBusy(true);
      void createTopic({ type, source: source.trim() })
        .then((out) => onCreated(out.topic))
        .catch((failure: unknown) => setError(failure instanceof Error ? failure.message : String(failure)))
        .finally(() => { setPending(false); onBusy(false); });
    }}>
      <div className="threads-create-body">
        <p className="threads-note">{t("Keep agent coordination connected to an existing project or fleet.")}</p>
        <div className="threads-form-field">
          <label className="threads-field-label" htmlFor={`${id}-scope`}>{t("Group scope")}</label>
          <Select id={`${id}-scope`} full value={type} disabled={pending || writeBlock !== null}
            onChange={(value) => { if (value === "project" || value === "fleet") setType(value); }}
            options={[{ value: "project", label: t("Project") }, { value: "fleet", label: t("Fleet") }]} />
        </div>
        <label className="threads-form-field" htmlFor={`${id}-source`}>
          <span className="threads-field-label">{type === "project" ? t("Project key") : t("Fleet name")}</span>
          <input className="field" id={`${id}-source`} value={source} disabled={pending || writeBlock !== null}
            onChange={(event) => setSource(event.target.value)} placeholder={type === "project" ? t("e.g. cadence") : t("Enter an existing fleet name")} />
        </label>
        {writeBlock && <p className="threads-note" role="status">{writeBlock}</p>}
        {error && <p className="threads-create-error" role="alert">{error}</p>}
      </div>
      <div className="threads-create-footer"><span className="threads-note">{t("Workspace access still applies.")}</span>
        <div><Button variant="secondary" onClick={onCancel} disabled={pending}>{t("Cancel")}</Button>
          <Button variant="primary" type="submit" disabled={pending || writeBlock !== null || source.trim() === ""}>{pending ? t("Creating…") : t("Create conversation")}</Button></div>
      </div>
    </form>
  );
}
