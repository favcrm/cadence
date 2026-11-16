import { useCallback, useEffect, useRef, useState } from "react";
import { ApiError } from "../../lib/api";
import { fmtTime } from "../../lib/fmt";
import Button from "../../ui/Button";
import Composer from "../app-shell/chat/Composer";
import { newMessageId } from "../home/thread";
import { ThreadComposerFrame, Turn } from "./ThreadChatParts";
import CredentialRequestList from "../connections/CredentialRequestCard";
import {
  addHumanMember,
  createHumanGroup,
  humanAgents,
  humanAskRoom,
  humanMe,
  humanPeople,
  listHumanRooms,
  markHumanRead,
  postHumanRoom,
  readHumanRoom,
  removeHumanMember,
  type HumanAgent,
  type HumanMember,
  type HumanMe,
  type HumanPerson,
  type HumanRoomPage,
  type HumanRoomSummary,
} from "./humanThreadsClient";

/**
 * Human DMs and human groups inside the Threads page (CAD-1412). These are
 * extra list entries and a timeline on the same page, reusing the Threads
 * shell, composer and bubble. The daemon checks identity and membership; this
 * view shows what the daemon returns and never decides authority.
 *
 * Mixed human/agent rooms are not offered: the agent Ask path is not ready,
 * and a durable agent UID is needed to add one. No generic message_send.
 * Private rooms are read by this authorized poll, not the shared SSE feed.
 */

/** Authorized poll for the list and the open room (no private SSE events). */
export const HUMAN_POLL_MS = 20_000;
const PAGE = 50;

const KIND_LABEL: Record<HumanRoomSummary["kind"], string> = {
  dm: "DM",
  human_group: "Group",
  mixed_group: "Mixed",
};

export function messageOf(e: unknown): string {
  return e instanceof ApiError || e instanceof Error ? e.message : String(e);
}

/** The room's current members as the timeline last read them (parent state). */
export interface HumanRoomState {
  id: string;
  /** Null when this person can no longer read the room. */
  members: HumanMember[] | null;
  denied: boolean;
}

/** A read refused for this room only: not a member, or the room is gone. */
function isDenial(e: unknown): boolean {
  return e instanceof ApiError && (e.status === 403 || e.message === "Room not found");
}

function isCursorRefusal(e: unknown): boolean {
  return e instanceof ApiError && (
    e.code === "human_room_cursor_invalid"
    || e.code === "human_room_cursor_numeric"
    || e.code === "human_room_cursor_format"
  );
}

/** The signed-in person from the server. `loading` until the first answer. */
export function useHumanMe(enabled: boolean): { me: HumanMe | null; error: string | null; loading: boolean } {
  const [state, setState] = useState<{ me: HumanMe | null; error: string | null; loading: boolean }>({
    me: null,
    error: null,
    loading: enabled,
  });
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    setState({ me: null, error: null, loading: true });
    humanMe()
      .then((me) => live && setState({ me, error: null, loading: false }))
      .catch((e: unknown) => live && setState({ me: null, error: messageOf(e), loading: false }));
    return () => {
      live = false;
    };
  }, [enabled]);
  return state;
}

/** The people on the trusted roster, each with the subject the daemon checks. */
export function useHumanPeople(enabled: boolean): { people: HumanPerson[] | null; error: string | null } {
  const [state, setState] = useState<{ people: HumanPerson[] | null; error: string | null }>({ people: null, error: null });
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    humanPeople()
      .then((people) => live && setState({ people, error: null }))
      .catch((e: unknown) => live && setState({ people: null, error: messageOf(e) }));
    return () => {
      live = false;
    };
  }, [enabled]);
  return state;
}

/** The current agents this person may ask. Unavailable endpoint: an empty list, no UIDs guessed. */
export function useHumanAgents(enabled: boolean): { agents: HumanAgent[] | null; error: string | null } {
  const [state, setState] = useState<{ agents: HumanAgent[] | null; error: string | null }>({ agents: null, error: null });
  useEffect(() => {
    if (!enabled) return;
    let live = true;
    humanAgents()
      .then((agents) => live && setState({ agents, error: null }))
      .catch((e: unknown) => live && setState({ agents: null, error: messageOf(e) }));
    return () => {
      live = false;
    };
  }, [enabled]);
  return state;
}

/** The human rooms list, refreshed on the authorized interval. */
export function useHumanRooms(enabled: boolean) {
  const [rooms, setRooms] = useState<HumanRoomSummary[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const latest = useRef(0);
  const load = useCallback(async () => {
    const n = ++latest.current;
    try {
      const out = await listHumanRooms();
      if (n === latest.current) {
        setRooms(out);
        setError(null);
      }
    } catch (e) {
      if (n === latest.current) setError(messageOf(e));
    }
  }, []);
  useEffect(() => {
    if (!enabled) return;
    void load();
    const timer = window.setInterval(() => void load(), HUMAN_POLL_MS);
    return () => window.clearInterval(timer);
  }, [enabled, load]);
  return { rooms, error, reload: load };
}

/** The list entries for human rooms. Each is a link that opens its room. */
export function HumanRoomRows({
  rooms,
  openId,
  hrefFor,
}: {
  rooms: HumanRoomSummary[];
  openId: string | null;
  hrefFor: (id: string) => string;
}) {
  return (
    <>
      {rooms.map((room) => (
        <li key={room.id}>
          <a
            href={hrefFor(room.id)}
            aria-current={room.id === openId ? "true" : undefined}
            className={room.id === openId ? "threads-row on" : "threads-row"}
          >
            <span className="threads-row-title">{room.title.trim() || (room.kind === "dm" ? "Direct message" : "Group")}</span>
            <span className="threads-row-meta">
              <span className="threads-row-kind">{KIND_LABEL[room.kind]}</span>
              {room.unread > 0 ? <span>{`${room.unread} unread`}</span> : null}
            </span>
          </a>
        </li>
      ))}
    </>
  );
}

/** Retain a loaded prefix only when the fresh tail overlaps it. */
function mergePage(cur: HumanRoomPage | null, next: HumanRoomPage): HumanRoomPage {
  if (!cur || cur.room !== next.room || cur.scope_id !== next.scope_id || next.posts.length === 0) return next;
  const positions = new Map(cur.posts.map((post, index) => [post.id, index]));
  const shared = next.posts.find((post) => positions.has(post.id));
  if (!shared) return next;
  const prefix = cur.posts.slice(0, positions.get(shared.id));
  const posts = [...prefix, ...next.posts];
  const sameOldest = posts[0]?.id === cur.posts[0]?.id;
  return {
    ...next,
    before: sameOldest ? cur.before : next.before,
    more_before: sameOldest ? cur.more_before : next.more_before,
    posts,
  };
}

/** An older page joins the loaded window; current members and tail stay. */
function mergeOlder(cur: HumanRoomPage, older: HumanRoomPage): HumanRoomPage {
  const currentIds = new Set(cur.posts.map((post) => post.id));
  return {
    ...cur,
    before: older.before,
    more_before: older.more_before,
    posts: [...older.posts.filter((post) => !currentIds.has(post.id)), ...cur.posts],
  };
}

/** An open human room: posts, earlier paging, composer and read marks. */
export function HumanRoomTimeline({
  id,
  me,
  refresh,
  onState,
  onOpenContext,
  onChanged,
  isDm,
}: {
  id: string;
  me: HumanMe;
  /** Bumped by the page after a Context change, so members re-read at once. */
  refresh: number;
  /** Reports the room's members (or its denial) to the page's Context. */
  onState: (state: HumanRoomState) => void;
  onOpenContext: () => void;
  onChanged: () => void;
  /** A direct message: a plain composer, no Post/Ask bar. */
  isDm: boolean;
}) {
  const [page, setPage] = useState<HumanRoomPage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [denied, setDenied] = useState(false);
  const [olderBusy, setOlderBusy] = useState(false);
  const [shown, setShown] = useState<string | null>(null);
  const [readError, setReadError] = useState<string | null>(null);
  const [cursorBlocked, setCursorBlocked] = useState(false);
  const [visibleId, setVisibleId] = useState<string | null>(null);
  const [documentVisible, setDocumentVisible] = useState(() => document.visibilityState === "visible");
  const [askTarget, setAskTarget] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const agentsState = useHumanAgents(true);
  // Ask is offered only when the fresh room hint allows it and the target is a current agent.
  const catalog = agentsState.agents ?? [];
  const currentPage = page?.room === id ? page : null;
  const askable = currentPage?.can_ask === true ? catalog.filter((a) => currentPage.members.some((m) => m.kind === "agent" && m.id === a.uid)) : [];
  const ask = askable.find((a) => a.uid === askTarget) ?? null;
  // Only the newest read for this room publishes; an older late reply is dropped.
  const latest = useRef(0);
  const visit = useRef(0);
  const envelope = useRef<{ key: string; id: string } | null>(null);
  const activeRoom = useRef(id);
  activeRoom.current = id;
  const pageRef = useRef(currentPage);
  pageRef.current = currentPage;
  const onStateRef = useRef(onState);
  onStateRef.current = onState;
  const onChangedRef = useRef(onChanged);
  onChangedRef.current = onChanged;
  const cursorBlockedRef = useRef(false);
  const blockHistory = useCallback((error: unknown) => {
    latest.current++;
    cursorBlockedRef.current = true;
    setCursorBlocked(true);
    setPage(null);
    setError(messageOf(error));
    onStateRef.current({ id, members: null, denied: false });
  }, [id]);

  const load = useCallback(async () => {
    if (cursorBlockedRef.current) return;
    const n = ++latest.current;
    try {
      let next = await readHumanRoom(id, { limit: PAGE });
      if (n !== latest.current) return;
      const previous = pageRef.current;
      if (previous?.room === id && previous.scope_id === next.scope_id && previous.posts.length > 0) {
        const loadedIds = new Set(previous.posts.map((post) => post.id));
        let oldest = next;
        const earlier: HumanRoomPage["posts"][] = [];
        while (!oldest.posts.some((post) => loadedIds.has(post.id)) && oldest.more_before) {
          if (!oldest.before) throw new Error("The room returned an invalid history cursor");
          const older = await readHumanRoom(id, { before: oldest.before, limit: PAGE });
          if (n !== latest.current) return;
          if (older.scope_id !== next.scope_id) {
            blockHistory(new Error("Room access changed while loading history"));
            return;
          }
          earlier.push(older.posts);
          oldest = older;
        }
        if (earlier.length > 0) {
          next = {
            ...next,
            before: oldest.before,
            more_before: oldest.more_before,
            posts: [...earlier.reverse().flat(), ...next.posts],
          };
        }
      }
      setPage((cur) => mergePage(cur, next));
      setDenied(false);
      setError(null);
      onStateRef.current({ id, members: next.members, denied: false });
    } catch (e) {
      if (n !== latest.current) return;
      if (isCursorRefusal(e)) {
        blockHistory(e);
        return;
      }
      if (isDenial(e)) {
        setPage(null);
        setDenied(true);
        onStateRef.current({ id, members: null, denied: true });
      } else {
        setError(messageOf(e));
      }
    }
  }, [id, blockHistory]);

  // A new room resets the timeline and starts its own authorized poll.
  useEffect(() => {
    visit.current++;
    cursorBlockedRef.current = false;
    setCursorBlocked(false);
    setPage(null);
    setShown(null);
    setVisibleId(null);
    setReadError(null);
    setError(null);
    setDenied(false);
    setOlderBusy(false);
    void load();
    const timer = window.setInterval(() => void load(), HUMAN_POLL_MS);
    return () => {
      visit.current++;
      latest.current++;
      window.clearInterval(timer);
    };
  }, [id, load]);

  useEffect(() => {
    if (refresh > 0) void load();
  }, [refresh, load]);

  const loadEarlier = () => {
    if (!currentPage?.before || !currentPage.more_before || olderBusy) return;
    const before = currentPage.before;
    const scope = currentPage.scope_id;
    const room = id;
    const requestVisit = visit.current;
    const stillCurrent = () => activeRoom.current === room && visit.current === requestVisit;
    setOlderBusy(true);
    readHumanRoom(room, { before, limit: PAGE })
      .then((older) => {
        if (!stillCurrent()) return;
        if (older.scope_id !== scope) {
          blockHistory(new Error("Room access changed while loading history"));
          return;
        }
        setPage((cur) => (cur?.room === room && cur.scope_id === scope && cur.before === before ? mergeOlder(cur, older) : cur));
      })
      .catch((e: unknown) => {
        if (!stillCurrent()) return;
        if (isCursorRefusal(e)) blockHistory(e);
        else setError(messageOf(e));
      })
      .finally(() => {
        if (stillCurrent()) setOlderBusy(false);
      });
  };

  // Only an actually visible post can advance the durable read marker.
  const messagesRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const changed = () => setDocumentVisible(document.visibilityState === "visible");
    document.addEventListener("visibilitychange", changed);
    return () => document.removeEventListener("visibilitychange", changed);
  }, []);
  useEffect(() => {
    const root = messagesRef.current;
    if (!root || !currentPage || typeof IntersectionObserver === "undefined") return;
    const candidates = new Set<HTMLElement>();
    const positions = new Map(currentPage.posts.map((post, index) => [post.id, index]));
    const measure = () => {
      const viewport = root.getBoundingClientRect();
      let newest = -1;
      for (const element of candidates) {
        const bounds = element.getBoundingClientRect();
        const overlap = Math.max(0, Math.min(bounds.bottom, viewport.bottom) - Math.max(bounds.top, viewport.top));
        const required = Math.min(bounds.height, viewport.height) * 0.5;
        const index = positions.get(element.dataset.postId ?? "");
        if (required > 0 && overlap >= required && index !== undefined && index > newest) newest = index;
      }
      setVisibleId(newest < 0 ? null : currentPage.posts[newest].id);
    };
    const observer = new IntersectionObserver((entries) => {
      for (const entry of entries) {
        const element = entry.target as HTMLElement;
        if (entry.isIntersecting) candidates.add(element);
        else candidates.delete(element);
      }
      measure();
    }, { root, threshold: 0 });
    root.querySelectorAll<HTMLElement>("[data-post-id]").forEach((el) => observer.observe(el));
    root.addEventListener("scroll", measure, { passive: true });
    window.addEventListener("resize", measure);
    const resize = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    resize?.observe(root);
    return () => {
      observer.disconnect();
      resize?.disconnect();
      root.removeEventListener("scroll", measure);
      window.removeEventListener("resize", measure);
    };
  }, [currentPage]);
  useEffect(() => {
    if (!documentVisible || document.visibilityState !== "visible" || !currentPage || !visibleId) return;
    const index = currentPage.posts.findIndex((post) => post.id === visibleId);
    const shownIndex = currentPage.posts.findIndex((post) => post.id === shown);
    if (index < 0 || index <= shownIndex) return;
    const post = currentPage.posts[index];
    let live = true;
    void markHumanRead(id, post.read_cursor)
      .then(() => {
        if (!live) return;
        setShown(post.id);
        setReadError(null);
        onChangedRef.current();
      })
      .catch((e: unknown) => {
        if (!live) return;
        if (isCursorRefusal(e)) blockHistory(e);
        else setReadError(messageOf(e));
      });
    return () => { live = false; };
  }, [currentPage, visibleId, shown, id, documentVisible, blockHistory]);

  return (
    <div className="threads-timeline">
      <header className="threads-head">
        <div className="threads-kicker">Human room</div>
        <h2>{currentPage ? currentPage.members.map((m) => m.label).join(" ↔ ") : "Loading…"}</h2>
        <div className="threads-head-actions">
          <Button variant="ghost" size="sm" onClick={onOpenContext} className="threads-context-toggle">
            Context
          </Button>
        </div>
      </header>
      {denied ? (
        <p className="threads-note" role="status">You are not a member of this room.</p>
      ) : error !== null ? (
        <p className="threads-note" role="status">This room could not load: {error}</p>
      ) : currentPage === null ? (
        <p className="threads-note">Loading…</p>
      ) : null}
      {readError !== null && <p className="threads-note" role="status">Read status could not be updated: {readError}</p>}
      {cursorBlocked && (
        <Button variant="ghost" size="sm" onClick={() => {
          cursorBlockedRef.current = false;
          setCursorBlocked(false);
          setError(null);
          void load();
        }}>
          Reload room
        </Button>
      )}
      <div className="threads-messages" aria-live="polite" ref={messagesRef}>
        <CredentialRequestList conversationId={id} />
        {currentPage?.more_before && currentPage.posts.length > 0 && (
          <Button variant="ghost" size="sm" disabled={olderBusy} onClick={loadEarlier}>
            Earlier
          </Button>
        )}
        {currentPage?.posts.map((p) => {
          const mine = p.sender.id === me.id;
          return (
            <article key={p.id} className="threads-msg" data-post-id={p.id} data-role={mine ? "operator" : "member"}>
              <Turn who={mine ? "You" : p.sender.label} at={fmtTime(p.created)} me={mine}>
                {p.body}
              </Turn>
            </article>
          );
        })}
        {currentPage && currentPage.posts.length === 0 && <p className="threads-note">No posts yet.</p>}
      </div>
      <ThreadComposerFrame
        showModes={!isDm}
        modeLabel="Message type"
        allowAsk={askable.length > 0}
        mode={ask ? "ask" : "post"}
        onMode={(m) => setAskTarget(m === "ask" ? askable[0]?.uid ?? null : null)}
        targetLabel="Agent to ask"
        target={ask?.uid ?? ""}
        onTarget={setAskTarget}
        targetOptions={askable.map((a) => ({ id: a.uid, label: a.alias }))}
        targetPlaceholder="Choose an agent"
        helper={ask ? "One explicit request to the agent you choose. Nothing else is sent." : "A post notifies members. It does not start agents."}
        notice={notice}
      >
        <Composer
          density="full"
          block={denied ? "You are not a member of this room" : null}
          storeKey={`threads-human:${id}`}
          onSend={async ({ body }) => {
            // One client id per logical envelope: a retry of the same mode, target and body reuses it.
            const key = JSON.stringify({ ask: ask?.uid ?? null, body: body.trim() });
            if (envelope.current?.key !== key) envelope.current = { key, id: crypto.randomUUID() };
            if (ask) {
              const receipt = await humanAskRoom(id, { agent_uid: ask.uid, client_id: envelope.current.id, body });
              setNotice(receipt.duplicate ? "This request was already queued." : `Queued for ${receipt.target.alias}. Not done yet.`);
            } else {
              await postHumanRoom(id, envelope.current.id, body);
            }
            envelope.current = null;
            await load();
          }}
          refocusAfterSend
          ariaLabel="Post to room"
          placeholder="Post to this room…"
          className="card p-2.5 relative"
        />
      </ThreadComposerFrame>
    </div>
  );
}

/** Roster people other than the signed-in person, or the reason there are none. */
function PersonNote({ people, error }: { people: HumanPerson[] | null; error: string | null }) {
  if (people !== null) return null;
  return <p className="threads-note" role="status">{error ? `People unavailable: ${error}` : "Loading people…"}</p>;
}

/** The Context of a human room: members, creator actions and the DM's Create group. */
export function HumanContextPanel({
  room,
  me,
  members,
  denied,
  people,
  peopleError,
  agents,
  onChanged,
  onOpened,
}: {
  room: HumanRoomSummary;
  me: HumanMe;
  members: HumanMember[] | null;
  denied: boolean;
  people: HumanPerson[] | null;
  peopleError: string | null;
  /** The current trusted agent catalog, or null while loading or unavailable. */
  agents: HumanAgent[] | null;
  onChanged: () => void;
  onOpened: (id: string) => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState("");
  const [agentPick, setAgentPick] = useState("");
  const [addAgentPick, setAddAgentPick] = useState("");
  const envelope = useRef<{ key: string; id: string } | null>(null);
  if (members === null) {
    return (
      <div className="threads-context-body">
        <p className="threads-note">{denied ? "No access to this room." : "Loading participants…"}</p>
      </div>
    );
  }
  // Groups are human or mixed; a DM is the fixed pair and never gets member edits.
  const isGroup = room.kind === "human_group" || room.kind === "mixed_group";
  const isCreator = room.creator === me.id;
  const act = async (fn: () => Promise<unknown>) => {
    setError(null);
    try {
      await fn();
      onChanged();
    } catch (e) {
      setError(messageOf(e));
    }
  };
  // Roster people who are not yet in this room and not the signed-in person.
  const addable = (people ?? []).filter((p) => p.id !== me.id && !members.some((m) => m.id === p.id));
  // Agents on the current catalog that are not already members (by durable uid).
  const addableAgents = (agents ?? []).filter((a) => !members.some((m) => m.kind === "agent" && m.id === a.uid));
  // A DM's fixed pair becomes a fresh group: the same members, no copied history.
  const createFromDm = () => {
    const humans = members
      .filter((m) => m.kind === "human" && m.id !== me.id)
      .map((m) => people?.find((p) => p.id === m.id)?.subject);
    if (humans.some((s) => s === undefined)) {
      setError("A member is not on the current roster");
      return;
    }
    setError(null);
    const subjects = humans as string[];
    const key = JSON.stringify(subjects);
    if (envelope.current?.key !== key) envelope.current = { key, id: newMessageId() };
    createHumanGroup({ kind: "human_group", client_id: envelope.current.id, humans: subjects, agents: [] })
      .then((id) => {
        envelope.current = null;
        onOpened(id);
      })
      .catch((e: unknown) => setError(messageOf(e)));
  };
  return (
    <div className="threads-context-body">
      <div className="threads-context-heading">
        <h3 className="threads-context-title">Participants</h3>
        <span className="threads-context-count">{members.length}</span>
      </div>
      <ul className="threads-roster">
        {members.map((m) => (
          <li key={`${m.kind}:${m.id}`} data-kind={m.kind}>
            <span className="threads-person">
              <span aria-hidden className="threads-avatar">{m.label.trim().charAt(0).toUpperCase() || "?"}</span>
              <span>{m.label}</span>
            </span>
            {isGroup && isCreator && m.kind === "human" && m.id !== me.id && (
              <Button variant="ghost" size="sm" onClick={() => void act(() => removeHumanMember(room.id, "human", m.id))}>
                Remove
              </Button>
            )}
            {isGroup && isCreator && m.kind === "agent" && (
              <Button variant="ghost" size="sm" onClick={() => void act(() => removeHumanMember(room.id, "agent", m.id))}>
                Remove
              </Button>
            )}
          </li>
        ))}
      </ul>
      {room.kind === "dm" && (
        <Button variant="secondary" size="sm" disabled={people === null} onClick={createFromDm}>
          Create group
        </Button>
      )}
      {(room.kind === "dm" || (isGroup && isCreator)) && (agents ?? []).length > 0 && (
        <form
          className="threads-add"
          aria-label="New mixed group with agent"
          onSubmit={(e) => {
            e.preventDefault();
            const agent = (agents ?? []).find((a) => a.uid === agentPick);
            if (!agent) return;
            // A new mixed room: current human members, the agent, no history copy. The original room is untouched.
            const humans = members.filter((m) => m.kind === "human" && m.id !== me.id).map((m) => people?.find((p) => p.id === m.id)?.subject);
            if (humans.some((s) => s === undefined)) {
              setError("A member is not on the current roster");
              return;
            }
            setError(null);
            const subjects = humans as string[];
            const key = JSON.stringify({ humans: subjects, agent: agent.uid });
            if (envelope.current?.key !== key) envelope.current = { key, id: newMessageId() };
            createHumanGroup({ kind: "mixed_group", client_id: envelope.current.id, humans: subjects, agents: [agent.uid] })
              .then((id) => {
                envelope.current = null;
                onOpened(id);
              })
              .catch((err: unknown) => setError(messageOf(err)));
          }}
        >
          <label>
            <span className="threads-field-label">New group with an agent</span>
            <select value={agentPick} onChange={(e) => setAgentPick(e.target.value)}>
              <option value="">Choose an agent</option>
              {(agents ?? []).map((a) => (
                <option key={a.uid} value={a.uid}>{a.alias}</option>
              ))}
            </select>
          </label>
          <Button variant="secondary" size="sm" type="submit" disabled={agentPick === ""}>New mixed group</Button>
        </form>
      )}
      {room.kind === "mixed_group" && isCreator && addableAgents.length > 0 && (
        <form
          className="threads-add"
          aria-label="Add agent to group"
          onSubmit={(e) => {
            e.preventDefault();
            if (addAgentPick === "") return;
            void act(async () => {
              await addHumanMember(room.id, { agent_uid: addAgentPick });
              setAddAgentPick("");
            });
          }}
        >
          <label>
            <span className="threads-field-label">Add an agent</span>
            <select value={addAgentPick} onChange={(e) => setAddAgentPick(e.target.value)}>
              <option value="">Choose an agent</option>
              {addableAgents.map((a) => (
                <option key={a.uid} value={a.uid}>{a.alias}</option>
              ))}
            </select>
          </label>
          <Button variant="secondary" size="sm" type="submit" disabled={addAgentPick === ""}>Add</Button>
        </form>
      )}
      {isGroup && isCreator && (
        <form
          className="threads-add"
          aria-label="Add person to group"
          onSubmit={(e) => {
            e.preventDefault();
            const subject = people?.find((p) => p.id === picked)?.subject;
            if (!subject) return;
            void act(async () => {
              await addHumanMember(room.id, { human_subject: subject });
              setPicked("");
            });
          }}
        >
          <label>
            <span className="threads-field-label">Add a person</span>
            <select value={picked} onChange={(e) => setPicked(e.target.value)}>
              <option value="">Choose a person</option>
              {addable.map((p) => (
                <option key={p.id} value={p.id}>{p.name}</option>
              ))}
            </select>
          </label>
          <Button variant="secondary" size="sm" type="submit" disabled={picked === ""}>Add</Button>
        </form>
      )}
      <PersonNote people={people} error={peopleError} />
      {error && <p className="threads-note" role="status">{error}</p>}
    </div>
  );
}

