import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ApiError } from "../../lib/api";
import { fmtTime } from "../../lib/fmt";
import { useTranslate } from "../../lib/locale";
import Button from "../../ui/Button";
import { IconArrowDown } from "../../ui/icons";
import Link from "../../ui/Link";
import Select from "../../ui/Select";
import Composer from "../app-shell/chat/Composer";
import ConfirmDialog from "../app-shell/shared/ConfirmDialog";
import { newMessageId } from "../home/thread";
import CredentialRequestList from "../connections/CredentialRequestCard";
import AgentAvatar, { lookFor } from "../agents/AgentAvatar";
import {
  slugifyHandle,
  startsWithNote,
  type MentionCandidate,
  type MentionOutcome,
} from "../mentions/mentions";
import { Turn } from "./ThreadChatParts";
import {
  HUMAN_LIVE_POLL_MS,
  HUMAN_POLL_MS,
  isHumanThreadsStreamLive,
  subscribeHumanThreadsStream,
} from "./humanThreadsStream";
import {
  addHumanMember,
  createHumanGroup,
  humanAgents,
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
  type HumanRoomEvent,
  type HumanRoomPage,
  type HumanRoomSummary,
} from "./humanThreadsClient";

/**
 * CAD-1412 / CAD-2711 / CAD-2765: Channels (1:1 DMs and shared teammate+agent channels).
 * Member-scoped `/api/human-threads/stream` invalidation delivers real-time updates
 * without placing transcripts on the wire, falling back to authorized polling on 404.
 * All routing and membership authority lives in the daemon; UI renders server
 * outcomes, `@mention` hints, Human-Turn Brake banners, agent avatars, and audited confirmations.
 */

export { HUMAN_POLL_MS };
const PAGE = 50;

const KIND_LABEL: Record<HumanRoomSummary["kind"], string> = {
  dm: "DM",
  human_group: "Channel",
  mixed_group: "Channel",
};

export function messageOf(e: unknown): string {
  return e instanceof ApiError || e instanceof Error ? e.message : String(e);
}

export interface HumanRoomState {
  id: string;
  /** Null when this person can no longer read the room. */
  members: HumanMember[] | null;
  denied: boolean;
  canManage: boolean;
  agentChainCount: number;
  agentChainLimit: number;
}

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
    let pollMs = isHumanThreadsStreamLive() ? HUMAN_LIVE_POLL_MS : HUMAN_POLL_MS;
    let timer = window.setInterval(() => void load(), pollMs);
    const restart = (nextMs: number) => {
      if (nextMs === pollMs) return;
      window.clearInterval(timer);
      pollMs = nextMs;
      timer = window.setInterval(() => void load(), pollMs);
    };
    const unsubscribe = subscribeHumanThreadsStream((msg) => {
      if (msg.kind === "open") {
        restart(HUMAN_LIVE_POLL_MS);
        if (msg.reconnected) void load();
        return;
      }
      if (msg.kind === "degraded") {
        restart(HUMAN_POLL_MS);
        return;
      }
      if (msg.scopes.includes("rooms")) void load();
    });
    return () => {
      window.clearInterval(timer);
      unsubscribe();
    };
  }, [enabled, load]);
  return { rooms, error, reload: load };
}

export interface BoardAgentHint {
  alias: string;
  look?: string | null;
  params?: Record<string, unknown> | null;
}

function cleanAgentAlias(label: string): string {
  const trimmed = label.trim();
  return trimmed.startsWith("@") ? trimmed.slice(1) : trimmed;
}

function resolveBoardAgentLook(agent: BoardAgentHint): string | undefined {
  if (typeof agent.look === "string" && agent.look) return agent.look;
  const paramLook = agent.params?.look;
  return typeof paramLook === "string" && paramLook ? paramLook : undefined;
}

function MemberAvatar({
  kind,
  label,
  look,
  size = 26,
}: {
  kind: "human" | "agent";
  label: string;
  look?: string | null;
  size?: number;
}) {
  if (kind === "agent") {
    const alias = cleanAgentAlias(label) || "agent";
    return <AgentAvatar slug={lookFor(alias)} look={look ?? undefined} still size={size} />;
  }
  const initial = label.trim().charAt(0).toUpperCase() || "?";
  return (
    <span
      aria-hidden
      className="threads-avatar"
      data-kind="human"
      style={{ width: size, height: size }}
    >
      {initial}
    </span>
  );
}

export function ConversationListSkeleton() {
  const t = useTranslate();
  return (
    <div className="threads-skel-group threads-skel-list" role="status" aria-busy="true">
      <span className="sr-only">{t("Loading conversations…")}</span>
      {[
        { title: "64%", pill: "2.2rem" },
        { title: "52%", pill: "1.4rem" },
        { title: "72%", pill: "2.2rem" },
        { title: "46%", pill: "1.4rem" },
      ].map((item, idx) => (
        <div key={idx} className="threads-skel-row" aria-hidden="true">
          <span className="threads-skel threads-skel-sigil" />
          <span className="threads-skel threads-skel-pulse" style={{ width: item.title, height: "0.78rem" }} />
          <span className="threads-skel threads-skel-pulse" style={{ width: item.pill, height: "0.7rem", marginLeft: "auto" }} />
        </div>
      ))}
    </div>
  );
}

export function ChannelTimelineSkeleton() {
  const t = useTranslate();
  return (
    <div className="threads-skel-group threads-skel-timeline" role="status" aria-busy="true">
      <span className="sr-only">{t("Loading…")}</span>
      <div className="threads-skel-msg" aria-hidden="true">
        <span className="threads-skel threads-skel-circle threads-skel-pulse" />
        <div className="threads-skel-lines">
          <span className="threads-skel threads-skel-pulse" style={{ width: "6.5rem", height: "0.68rem" }} />
          <span className="threads-skel threads-skel-bubble threads-skel-pulse" style={{ width: "74%", height: "2.4rem" }} />
        </div>
      </div>
      <div className="threads-skel-msg is-me" aria-hidden="true">
        <span className="threads-skel threads-skel-bubble threads-skel-pulse" style={{ width: "56%", height: "2.25rem" }} />
      </div>
      <div className="threads-skel-msg" aria-hidden="true">
        <span className="threads-skel threads-skel-circle threads-skel-pulse" />
        <div className="threads-skel-lines">
          <span className="threads-skel threads-skel-pulse" style={{ width: "7.5rem", height: "0.68rem" }} />
          <span className="threads-skel threads-skel-bubble threads-skel-pulse" style={{ width: "84%", height: "3.25rem" }} />
        </div>
      </div>
    </div>
  );
}

export function DetailsPanelSkeleton() {
  const t = useTranslate();
  return (
    <div className="threads-context-body threads-skel-group" role="status" aria-busy="true">
      <span className="sr-only">{t("Loading participants…")}</span>
      <div className="threads-context-room" aria-hidden="true">
        <span className="threads-skel threads-skel-pulse" style={{ width: "2rem", height: "2rem", borderRadius: "0.45rem" }} />
        <div className="threads-skel-lines" style={{ flex: 1 }}>
          <span className="threads-skel threads-skel-pulse" style={{ width: "62%", height: "0.85rem" }} />
          <span className="threads-skel threads-skel-pulse" style={{ width: "42%", height: "0.68rem" }} />
        </div>
      </div>
      <div className="threads-context-section" aria-hidden="true">
        <span className="threads-skel threads-skel-pulse" style={{ width: "4.8rem", height: "0.68rem", marginBottom: "0.55rem" }} />
        <div className="threads-skel-roster">
          {[0, 1].map((idx) => (
            <div key={idx} className="threads-skel-member">
              <span className="threads-skel threads-skel-circle threads-skel-pulse" />
              <span className="threads-skel threads-skel-pulse" style={{ width: idx === 0 ? "58%" : "44%", height: "0.78rem" }} />
            </div>
          ))}
        </div>
      </div>
      <div className="threads-context-section" aria-hidden="true">
        <span className="threads-skel threads-skel-pulse" style={{ width: "5.5rem", height: "0.68rem", marginBottom: "0.55rem" }} />
        <div className="threads-skel-roster">
          <div className="threads-skel-member">
            <span className="threads-skel threads-skel-circle threads-skel-pulse" />
            <span className="threads-skel threads-skel-pulse" style={{ width: "50%", height: "0.78rem" }} />
          </div>
        </div>
      </div>
    </div>
  );
}

/** Client-routed channel & DM rows in the left sidebar. */
export function HumanRoomRows({
  rooms,
  openId,
  hrefFor,
}: {
  rooms: HumanRoomSummary[];
  openId: string | null;
  hrefFor: (id: string) => string;
}) {
  const t = useTranslate();
  return (
    <>
      {rooms.map((room) => {
        const title = room.title.trim() || (room.kind === "dm" ? t("Direct message") : t("Channel"));
        const isDm = room.kind === "dm";
        return (
          <li key={room.id}>
            <Link
              href={hrefFor(room.id)}
              aria-current={room.id === openId ? "true" : undefined}
              className={room.id === openId ? "threads-row on" : "threads-row"}
            >
              <span className="threads-row-top">
                <span className="threads-row-title">
                  <span className="threads-row-sigil" aria-hidden="true">
                    {isDm ? "@" : "#"}
                  </span>
                  {title}
                </span>
                {room.unread > 0 ? (
                  <span className="threads-unread-pill" aria-label={`${room.unread} ${t("unread")}`}>
                    {room.unread}
                  </span>
                ) : (
                  <span className="threads-row-kind">{t(KIND_LABEL[room.kind])}</span>
                )}
              </span>
            </Link>
          </li>
        );
      })}
    </>
  );
}

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

function mergeOlder(cur: HumanRoomPage, older: HumanRoomPage): HumanRoomPage {
  const currentIds = new Set(cur.posts.map((post) => post.id));
  return {
    ...cur,
    before: older.before,
    more_before: older.more_before,
    posts: [...older.posts.filter((post) => !currentIds.has(post.id)), ...cur.posts],
  };
}

function MentionOutcomes({ outcomes }: { outcomes: MentionOutcome[] }) {
  const t = useTranslate();
  if (outcomes.length === 0) return null;
  const labelFor = (o: MentionOutcome) => {
    switch (o.kind) {
      case "queued":
        return `${o.handle} · ${t("queued")}`;
      case "notified":
        return `${o.handle} · ${t("notified")}`;
      case "silent":
        return `${o.handle} · ${t("silent")}`;
      case "blocked_dm":
        return `${o.handle} · ${t("blocked in DM")}`;
      case "blocked_chain":
        return `${o.handle} · ${t("paused (turn limit)")}`;
      case "blocked_acl":
        return `${o.handle} · ${t("not in channel")}`;
      default:
        return `${o.handle} · ${o.kind}`;
    }
  };
  return (
    <div className="threads-outcomes" aria-label={t("Mention routing outcomes")}>
      {outcomes.map((o, idx) => (
        <span
          key={`${o.handle}-${o.kind}-${idx}`}
          className="threads-outcome"
          data-kind={o.kind}
          title={o.reason ?? o.label}
        >
          {labelFor(o)}
        </span>
      ))}
    </div>
  );
}

/** An open Channel or DM: header avatar stack, interleaved posts/events, `@mention` + `/note` floating dock. */
export function HumanRoomTimeline({
  id,
  me,
  refresh,
  onState,
  onOpenContext,
  detailsOpen = false,
  onInvite,
  onChanged,
  isDm,
  roomTitle,
  people,
  boardAgents,
}: {
  id: string;
  me: HumanMe;
  refresh: number;
  onState: (state: HumanRoomState) => void;
  onOpenContext: () => void;
  detailsOpen?: boolean;
  onInvite?: () => void;
  onChanged: () => void;
  isDm: boolean;
  roomTitle?: string;
  people?: HumanPerson[] | null;
  boardAgents?: readonly BoardAgentHint[] | null;
}) {
  const t = useTranslate();
  const [page, setPage] = useState<HumanRoomPage | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [denied, setDenied] = useState(false);
  const [olderBusy, setOlderBusy] = useState(false);
  const [shown, setShown] = useState<string | null>(null);
  const [readError, setReadError] = useState<string | null>(null);
  const [cursorBlocked, setCursorBlocked] = useState(false);
  const [visibleId, setVisibleId] = useState<string | null>(null);
  const [documentVisible, setDocumentVisible] = useState(() => document.visibilityState === "visible");
  const [silentNote, setSilentNote] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const agentsState = useHumanAgents(true);
  const catalog = useMemo(() => agentsState.agents ?? [], [agentsState.agents]);
  const currentPage = page?.room === id ? page : null;

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

  const blockHistory = useCallback((failure: unknown) => {
    latest.current++;
    cursorBlockedRef.current = true;
    setCursorBlocked(true);
    setPage(null);
    setError(messageOf(failure));
    onStateRef.current({
      id,
      members: null,
      denied: false,
      canManage: false,
      agentChainCount: 0,
      agentChainLimit: 0,
    });
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
      onStateRef.current({
        id,
        members: next.members,
        denied: false,
        canManage: next.can_manage,
        agentChainCount: next.agent_chain_count,
        agentChainLimit: next.agent_chain_limit,
      });
    } catch (e) {
      if (n !== latest.current) return;
      if (isCursorRefusal(e)) {
        blockHistory(e);
        return;
      }
      if (isDenial(e)) {
        setPage(null);
        setDenied(true);
        onStateRef.current({
          id,
          members: null,
          denied: true,
          canManage: false,
          agentChainCount: 0,
          agentChainLimit: 0,
        });
      } else {
        setError(messageOf(e));
      }
    }
  }, [id, blockHistory]);

  useEffect(() => {
    visit.current++;
    cursorBlockedRef.current = false;
    setCursorBlocked(false);
    setPage(null);
    setShown(null);
    setVisibleId(null);
    setReadError(null);
    setSendError(null);
    setSilentNote(false);
    setError(null);
    setDenied(false);
    setOlderBusy(false);
    void load();
    let pollMs = isHumanThreadsStreamLive() ? HUMAN_LIVE_POLL_MS : HUMAN_POLL_MS;
    let timer = window.setInterval(() => void load(), pollMs);
    const restart = (nextMs: number) => {
      if (nextMs === pollMs) return;
      window.clearInterval(timer);
      pollMs = nextMs;
      timer = window.setInterval(() => void load(), pollMs);
    };
    const unsubscribe = subscribeHumanThreadsStream((msg) => {
      if (msg.kind === "open") {
        restart(HUMAN_LIVE_POLL_MS);
        if (msg.reconnected) void load();
        return;
      }
      if (msg.kind === "degraded") {
        restart(HUMAN_POLL_MS);
        return;
      }
      if (msg.scopes.includes("room") && (!msg.room || msg.room === id)) {
        void load();
      }
    });
    return () => {
      visit.current++;
      latest.current++;
      window.clearInterval(timer);
      unsubscribe();
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

  const messagesRef = useRef<HTMLDivElement | null>(null);
  const pinnedToTailRef = useRef(true);
  const [pendingNewCount, setPendingNewCount] = useState(0);
  const tailPostRef = useRef<{ room: string; id: string | null; initialized: boolean }>({
    room: "",
    id: null,
    initialized: false,
  });
  useEffect(() => {
    pinnedToTailRef.current = true;
    tailPostRef.current = { room: id, id: null, initialized: false };
    setPendingNewCount(0);
  }, [id]);
  useEffect(() => {
    const root = messagesRef.current;
    if (!root) return;
    const onScroll = () => {
      const pinned = root.scrollHeight - root.scrollTop - root.clientHeight <= 48;
      pinnedToTailRef.current = pinned;
      if (pinned) setPendingNewCount(0);
    };
    root.addEventListener("scroll", onScroll, { passive: true });
    return () => root.removeEventListener("scroll", onScroll);
  }, []);
  useEffect(() => {
    const root = messagesRef.current;
    if (!root || !currentPage || currentPage.room !== id) return;
    const posts = currentPage.posts;
    const lastId = posts[posts.length - 1]?.id ?? null;
    const prev = tailPostRef.current;
    if (prev.initialized && prev.room === id && prev.id === lastId) return;
    tailPostRef.current = { room: id, id: lastId, initialized: true };
    if (pinnedToTailRef.current) {
      root.scrollTop = Math.max(0, root.scrollHeight - root.clientHeight);
      setPendingNewCount(0);
      return;
    }
    if (prev.initialized && prev.room === id) {
      const prevIdx = prev.id === null ? -1 : posts.findIndex((p) => p.id === prev.id);
      const added = prevIdx >= 0 ? posts.slice(prevIdx + 1) : posts;
      const incoming = added.filter((p) => p.sender.id !== me.id).length;
      if (incoming > 0) setPendingNewCount((count) => count + incoming);
    }
  }, [id, currentPage, me.id]);
  const jumpToLatest = () => {
    const root = messagesRef.current;
    pinnedToTailRef.current = true;
    setPendingNewCount(0);
    if (!root) return;
    root.scrollTop = Math.max(0, root.scrollHeight - root.clientHeight);
  };
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
      setVisibleId(newest < 0 ? null : currentPage.posts[newest]!.id);
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
    const post = currentPage.posts[index]!;
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

  const mentionCandidates = useMemo<MentionCandidate[]>(() => {
    if (!currentPage) return [];
    const out: MentionCandidate[] = [];
    const memberIds = new Set(currentPage.members.map((m) => `${m.kind}:${m.id}`));
    for (const m of currentPage.members) {
      if (m.kind === "human" && m.id === me.id) continue;
      if (m.kind === "human") {
        const handle = slugifyHandle(m.label) || m.id;
        out.push({
          id: m.id,
          handle,
          label: m.label,
          kind: "human",
          active: true,
        });
      } else {
        out.push({
          id: m.id,
          handle: m.label,
          label: m.label,
          kind: "agent",
          active: true,
        });
      }
    }
    for (const p of people ?? []) {
      if (p.id === me.id || memberIds.has(`human:${p.id}`)) continue;
      out.push({
        id: p.id,
        handle: slugifyHandle(p.name) || p.subject,
        label: p.name,
        kind: "human",
        active: false,
        unavailableReason: "Not in this channel",
      });
    }
    for (const a of catalog) {
      if (memberIds.has(`agent:${a.uid}`)) continue;
      out.push({
        id: a.uid,
        handle: a.alias,
        label: a.alias,
        kind: "agent",
        active: false,
        unavailableReason: isDm ? "Not available in 1:1 DMs" : "Not added to this channel",
      });
    }
    return out;
  }, [currentPage, me.id, people, catalog, isDm]);

  const timelineItems = useMemo(() => {
    if (!currentPage) return [];
    type Item =
      | { kind: "post"; key: string; created: number; post: HumanRoomPage["posts"][number] }
      | { kind: "event"; key: string; created: number; event: HumanRoomEvent };
    const merged: Item[] = [
      ...currentPage.posts.map((post) => ({ kind: "post" as const, key: `p:${post.id}`, created: post.created, post })),
      ...currentPage.events.map((event) => ({ kind: "event" as const, key: `e:${event.id}`, created: event.created, event })),
    ];
    merged.sort((a, b) => a.created - b.created || (a.kind === b.kind ? 0 : a.kind === "post" ? -1 : 1));
    return merged;
  }, [currentPage]);

  const brakePaused =
    currentPage !== null &&
    currentPage.agent_chain_limit > 0 &&
    currentPage.agent_chain_count >= currentPage.agent_chain_limit;

  const headerTitle = isDm
    ? currentPage
      ? currentPage.members.filter((m) => m.id !== me.id).map((m) => m.label).join(" ↔ ") ||
        currentPage.members.map((m) => m.label).join(" ↔ ")
      : roomTitle?.trim() || t("Direct message")
    : `# ${roomTitle?.trim() || t("Channel")}`;

  const memberCount = currentPage?.members.length ?? 0;
  const hasAgents = currentPage?.members.some((m) => m.kind === "agent") ?? false;
  const headerSub = !currentPage
    ? null
    : isDm
      ? t("Direct message")
      : `${memberCount} ${t(memberCount === 1 ? "member" : "members")}${hasAgents ? ` · ${t("Channel")}` : ""}`;

  const agentIdentity = useMemo(() => {
    const byId = new Map<string, { alias: string; look?: string | null }>();
    const byAlias = new Map<string, { id?: string; look?: string | null }>();
    for (const a of boardAgents ?? []) {
      const look = resolveBoardAgentLook(a);
      byAlias.set(a.alias.toLowerCase(), { look });
    }
    for (const a of catalog) {
      const prev = byAlias.get(a.alias.toLowerCase());
      const look = a.look ?? prev?.look ?? null;
      byId.set(a.uid, { alias: a.alias, look });
      byAlias.set(a.alias.toLowerCase(), { id: a.uid, look });
    }
    for (const m of currentPage?.members ?? []) {
      if (m.kind !== "agent") continue;
      const alias = cleanAgentAlias(m.label);
      const prev = byAlias.get(alias.toLowerCase()) ?? byId.get(m.id);
      const look = m.look ?? prev?.look ?? null;
      byId.set(m.id, { alias, look });
      byAlias.set(alias.toLowerCase(), { id: m.id, look });
    }
    return { byId, byAlias };
  }, [boardAgents, catalog, currentPage?.members]);

  const previewMembers = currentPage?.members.slice(0, 4) ?? [];
  const overflowMembers = Math.max(0, memberCount - previewMembers.length);

  return (
    <div className="threads-timeline">
      <header className="threads-head">
        <div className="threads-head-title">
          <h2>{headerTitle}</h2>
          {headerSub !== null ? (
            <div className="threads-kicker">{headerSub}</div>
          ) : !denied && error === null ? (
            <span className="threads-skel threads-skel-kicker threads-skel-pulse" aria-hidden="true" />
          ) : null}
        </div>
        <div className="threads-head-actions">
          {previewMembers.length > 0 && (
            <div
              className="threads-avatar-stack"
              title={currentPage?.members.map((m) => m.label).join(", ")}
              aria-label={`${memberCount} ${t(memberCount === 1 ? "member" : "members")}`}
            >
              {previewMembers.map((m) => {
                const look = m.kind === "agent"
                  ? (m.look ?? agentIdentity.byId.get(m.id)?.look ?? agentIdentity.byAlias.get(cleanAgentAlias(m.label).toLowerCase())?.look)
                  : undefined;
                return (
                  <MemberAvatar
                    key={`${m.kind}:${m.id}`}
                    kind={m.kind}
                    label={m.label}
                    look={look}
                    size={24}
                  />
                );
              })}
              {overflowMembers > 0 && <span className="threads-avatar-more">+{overflowMembers}</span>}
            </div>
          )}
          {!isDm && currentPage?.can_manage && onInvite && (
            <Button variant="secondary" size="sm" onClick={onInvite}>
              {t("+ Invite")}
            </Button>
          )}
          <Button
            variant="ghost"
            size="sm"
            onClick={onOpenContext}
            aria-expanded={detailsOpen}
            className="threads-context-toggle"
          >
            {t("Details")}
          </Button>
        </div>
      </header>
      {denied ? (
        <p className="threads-note px-6 py-3" role="status">{t("You are not a member of this channel.")}</p>
      ) : error !== null ? (
        <p className="threads-note px-6 py-3" role="status">{t("This channel could not load: {error}").replace("{error}", error)}</p>
      ) : null}
      {readError !== null && (
        <p className="threads-note px-6 py-2" role="status">
          {t("Read status could not be updated: {error}").replace("{error}", readError)}
        </p>
      )}
      {cursorBlocked && (
        <div className="px-6 py-2">
          <Button variant="ghost" size="sm" onClick={() => {
            cursorBlockedRef.current = false;
            setCursorBlocked(false);
            setError(null);
            void load();
          }}>
            {t("Reload channel")}
          </Button>
        </div>
      )}
      <div className="threads-messages" aria-live="polite" ref={messagesRef}>
        <CredentialRequestList conversationId={id} />
        {currentPage === null && !denied && error === null && <ChannelTimelineSkeleton />}
        {currentPage?.more_before && currentPage.posts.length > 0 && (
          <div className="flex justify-center py-2">
            <Button variant="ghost" size="sm" disabled={olderBusy} onClick={loadEarlier}>
              {t("Earlier")}
            </Button>
          </div>
        )}
        {timelineItems.map((item) => {
          if (item.kind === "event") {
            return (
              <div key={item.key} className="threads-audit-event" role="note">
                <span>{item.event.text}</span>
                <time className="num">{fmtTime(item.event.created)}</time>
              </div>
            );
          }
          const p = item.post;
          const mine = p.sender.id === me.id;
          const senderClean = cleanAgentAlias(p.sender.label);
          const agentById = p.sender.id ? agentIdentity.byId.get(p.sender.id) : undefined;
          const agentByAlias = agentIdentity.byAlias.get(senderClean.toLowerCase());
          const isAgent =
            p.sender.kind === "agent" ||
            (p.sender.kind !== "human" && (agentById !== undefined || agentByAlias !== undefined));
          const senderLook = isAgent ? (agentById?.look ?? agentByAlias?.look) : undefined;
          const who = mine
            ? t("You")
            : isAgent
              ? `@${senderClean || p.sender.label}`
              : p.sender.label;
          return (
            <article key={item.key} className="threads-msg" data-post-id={p.id} data-role={mine ? "operator" : "member"}>
              {p.silent && (
                <div className="threads-silent-row">
                  <span className="threads-silent-badge">{t("Silent note")}</span>
                </div>
              )}
              <Turn
                who={who}
                at={fmtTime(p.created)}
                me={mine}
                showName={!mine}
                markdown={isAgent && !mine}
                avatar={
                  !mine ? (
                    <MemberAvatar
                      kind={isAgent ? "agent" : "human"}
                      label={senderClean || p.sender.label}
                      look={senderLook}
                      size={28}
                    />
                  ) : undefined
                }
              >
                {p.body}
              </Turn>
              <MentionOutcomes outcomes={p.outcomes} />
            </article>
          );
        })}
        {currentPage && timelineItems.length === 0 && (
          <p className="threads-note py-6 text-center">{t("No messages yet.")}</p>
        )}
      </div>
      <div className="threads-composer-dock">
        {pendingNewCount > 0 && (
          <div className="threads-jump-wrap">
            <button type="button" className="threads-jump-pill" onClick={jumpToLatest}>
              <IconArrowDown size={13} />
              <span>
                {pendingNewCount === 1
                  ? t("1 new message")
                  : t("{count} new messages").replace("{count}", String(pendingNewCount))}
              </span>
            </button>
          </div>
        )}
        {brakePaused && (
          <div className="threads-brake-banner" role="status">
            {t("Channel paused after consecutive agent turns — send a message to continue.")}
          </div>
        )}
        {sendError && (
          <p className="threads-note text-fail" role="alert">
            {sendError}
          </p>
        )}
        <div className="threads-composer-card">
          <Composer
            density="full"
            block={denied ? t("You are not a member of this channel") : null}
            storeKey={`threads-human:${id}`}
            mentionCandidates={mentionCandidates}
            showSilentToggle={!isDm}
            silent={silentNote}
            onSilentChange={setSilentNote}
            onSend={async ({ body }) => {
              setSendError(null);
              const isSilent = silentNote || startsWithNote(body);
              const key = JSON.stringify({ silent: isSilent, body: body.trim() });
              if (envelope.current?.key !== key) envelope.current = { key, id: crypto.randomUUID() };
              try {
                await postHumanRoom(id, envelope.current.id, body, isSilent);
                envelope.current = null;
                setSilentNote(false);
                pinnedToTailRef.current = true;
                setPendingNewCount(0);
                await load();
              } catch (e) {
                setSendError(messageOf(e));
                throw e;
              }
            }}
            refocusAfterSend
            ariaLabel={isDm ? t("Message this direct message…") : t("Message this channel… (@mention teammates or agents)")}
            placeholder={
              isDm
                ? t("Message this direct message…")
                : silentNote
                  ? t("Silent note — stays in channel without waking or notifying…")
                  : t("Message this channel… (@mention teammates or agents)")
            }
            className="card p-2.5 relative"
          />
          {!isDm && (
            <p className="threads-dock-hint">
              {t("Mention @name to notify or @agent to run · /note for silent")}
            </p>
          )}
        </div>
      </div>
    </div>
  );
}

function PersonNote({ error }: { error: string | null }) {
  const t = useTranslate();
  if (!error) return null;
  return (
    <p className="threads-note" role="status">
      {t("People unavailable: {error}").replace("{error}", error)}
    </p>
  );
}

/** The Details rail / sheet for a Channel or DM: Teammates, Shared Agents, Active Guardrails, and Remove ConfirmDialog. */
export function HumanContextPanel({
  room,
  me,
  members,
  denied,
  canManage,
  agentChainCount = 0,
  agentChainLimit = 0,
  people,
  peopleError,
  agents,
  boardAgents,
  onChanged,
  onOpened,
}: {
  room: HumanRoomSummary;
  me: HumanMe;
  members: HumanMember[] | null;
  denied: boolean;
  canManage?: boolean;
  agentChainCount?: number;
  agentChainLimit?: number;
  people: HumanPerson[] | null;
  peopleError: string | null;
  agents: HumanAgent[] | null;
  boardAgents?: readonly BoardAgentHint[] | null;
  onChanged: () => void;
  onOpened: (id: string) => void;
}) {
  const t = useTranslate();
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState("");
  const [addAgentPick, setAddAgentPick] = useState("");
  const [memberFilter, setMemberFilter] = useState("");
  const [pendingRemove, setPendingRemove] = useState<HumanMember | null>(null);
  const [removing, setRemoving] = useState(false);
  const [removeError, setRemoveError] = useState<string | null>(null);
  const envelope = useRef<{ key: string; id: string } | null>(null);

  const agentLookLookup = useMemo(() => {
    const byUid = new Map<string, string>();
    const byAlias = new Map<string, string>();
    for (const a of boardAgents ?? []) {
      const look = resolveBoardAgentLook(a);
      if (look) byAlias.set(a.alias.toLowerCase(), look);
    }
    for (const a of agents ?? []) {
      const look = a.look ?? byAlias.get(a.alias.toLowerCase());
      if (look) {
        byUid.set(a.uid, look);
        byAlias.set(a.alias.toLowerCase(), look);
      }
    }
    return { byUid, byAlias };
  }, [agents, boardAgents]);

  if (members === null) {
    if (denied) {
      return (
        <div className="threads-context-body">
          <p className="threads-note">{t("No access to this channel.")}</p>
        </div>
      );
    }
    return <DetailsPanelSkeleton />;
  }

  const isGroup = room.kind === "human_group" || room.kind === "mixed_group";
  const mayManage = canManage ?? room.creator === me.id;
  const roomTitle = room.title.trim() || (room.kind === "dm" ? t("Direct message") : t("Channel"));

  const act = async (fn: () => Promise<unknown>) => {
    setError(null);
    try {
      await fn();
      onChanged();
    } catch (e) {
      setError(messageOf(e));
    }
  };

  const confirmRemoveMember = async () => {
    if (!pendingRemove) return;
    setRemoving(true);
    setRemoveError(null);
    try {
      await removeHumanMember(room.id, pendingRemove.kind, pendingRemove.id);
      setPendingRemove(null);
      onChanged();
    } catch (e) {
      setRemoveError(messageOf(e));
    } finally {
      setRemoving(false);
    }
  };

  const humanMembers = members.filter((m) => m.kind === "human");
  const agentMembers = members.filter((m) => m.kind === "agent");
  const addable = (people ?? []).filter((p) => p.id !== me.id && !members.some((m) => m.id === p.id));
  const addableAgents = (agents ?? []).filter((a) => !members.some((m) => m.kind === "agent" && m.id === a.uid));
  const query = memberFilter.trim().toLowerCase();
  const shownHumans = query === ""
    ? humanMembers
    : humanMembers.filter((m) => m.label.toLowerCase().includes(query));
  const brakePaused = agentChainLimit > 0 && agentChainCount >= agentChainLimit;

  const startSharedChannelFromDm = () => {
    const humans = members
      .filter((m) => m.kind === "human" && m.id !== me.id)
      .map((m) => people?.find((p) => p.id === m.id)?.subject);
    if (humans.some((s) => s === undefined)) {
      setError(t("A member is not on the current roster"));
      return;
    }
    setError(null);
    const subjects = humans as string[];
    const key = JSON.stringify(subjects);
    if (envelope.current?.key !== key) envelope.current = { key, id: newMessageId() };
    createHumanGroup({ kind: "human_group", client_id: envelope.current.id, humans: subjects, agents: [] })
      .then((nextRoom) => {
        envelope.current = null;
        onOpened(nextRoom);
      })
      .catch((e: unknown) => setError(messageOf(e)));
  };

  const lookForMember = (m: HumanMember): string | undefined =>
    m.kind === "agent"
      ? (m.look ?? agentLookLookup.byUid.get(m.id) ?? agentLookLookup.byAlias.get(cleanAgentAlias(m.label).toLowerCase()))
      : undefined;

  const renderMemberRow = (m: HumanMember) => {
    const canRemove =
      isGroup &&
      mayManage &&
      ((m.kind === "human" && m.id !== room.creator && m.id !== me.id) || m.kind === "agent");
    const displayLabel = m.kind === "agent" ? `@${cleanAgentAlias(m.label)}` : m.label;
    return (
      <li key={`${m.kind}:${m.id}`} data-kind={m.kind}>
        <span className="threads-person">
          <MemberAvatar
            kind={m.kind}
            label={m.label}
            look={lookForMember(m)}
            size={26}
          />
          <span className="threads-person-name">{displayLabel}</span>
        </span>
        {canRemove && (
          <Button
            variant="ghost"
            size="sm"
            className="threads-remove-btn"
            onClick={() => {
              setRemoveError(null);
              setPendingRemove(m);
            }}
          >
            {t("Remove")}
          </Button>
        )}
      </li>
    );
  };

  return (
    <div className="threads-context-body">
      <div className="threads-context-room">
        <span className="threads-context-room-badge" aria-hidden="true">
          {room.kind === "dm" ? "@" : "#"}
        </span>
        <div className="threads-context-room-meta">
          <strong className="threads-context-room-title">{roomTitle}</strong>
          <span className="threads-context-room-sub">
            {room.kind === "dm"
              ? t("Direct message")
              : `${members.length} ${t(members.length === 1 ? "member" : "members")}`}
          </span>
        </div>
      </div>

      <section className="threads-context-section" aria-label={t("Teammates")}>
        <div className="threads-context-heading">
          <h3 className="threads-context-title">{t("Teammates")}</h3>
          <span className="threads-context-count">{humanMembers.length}</span>
        </div>
        {humanMembers.length >= 5 && (
          <div className="threads-member-filter-wrap">
            <input
              type="search"
              className="field threads-member-filter"
              value={memberFilter}
              onChange={(e) => setMemberFilter(e.target.value)}
              placeholder={t("Filter members…")}
              aria-label={t("Filter members")}
            />
          </div>
        )}
        <ul className="threads-roster" data-roster-humans="">
          {shownHumans.map(renderMemberRow)}
          {shownHumans.length === 0 && query !== "" && (
            <li className="threads-note">{t("No members match “{query}”.").replace("{query}", memberFilter.trim())}</li>
          )}
        </ul>

        {room.kind === "dm" && (
          <div className="threads-add">
            <Button variant="secondary" size="sm" disabled={people === null} onClick={startSharedChannelFromDm}>
              {t("Start shared channel")}
            </Button>
          </div>
        )}

        {isGroup && mayManage && (
          <form
            className="threads-add threads-add-inline"
            aria-label={t("Invite teammate to channel")}
            data-invite-select=""
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
            <div className="threads-form-field">
              <Select
                full
                searchable
                size="sm"
                value={picked}
                onChange={setPicked}
                disabled={addable.length === 0}
                placeholder={
                  people === null && !peopleError
                    ? t("Loading teammates…")
                    : addable.length === 0
                      ? t("No people to add")
                      : t("+ Invite teammate…")
                }
                aria-label={t("Add a person")}
                options={addable.map((p) => ({ value: p.id, label: p.name }))}
              />
            </div>
            <Button variant="secondary" size="sm" type="submit" disabled={picked === ""}>{t("Add")}</Button>
          </form>
        )}
      </section>

      {isGroup && (
        <section className="threads-context-section" aria-label={t("Shared Agents")}>
          <div className="threads-context-heading">
            <h3 className="threads-context-title">{t("Shared Agents")}</h3>
            <span className="threads-context-count">{agentMembers.length}</span>
          </div>
          {agentMembers.length > 0 ? (
            <ul className="threads-roster" data-roster-agents="">
              {agentMembers.map(renderMemberRow)}
            </ul>
          ) : (
            <p className="threads-note">{t("No shared agents added yet.")}</p>
          )}
          {mayManage && (
            <form
              className="threads-add threads-add-inline"
              aria-label={t("Add agent to channel")}
              onSubmit={(e) => {
                e.preventDefault();
                if (addAgentPick === "") return;
                void act(async () => {
                  await addHumanMember(room.id, { agent_uid: addAgentPick });
                  setAddAgentPick("");
                });
              }}
            >
              <div className="threads-form-field">
                <Select
                  full
                  searchable
                  size="sm"
                  value={addAgentPick}
                  onChange={setAddAgentPick}
                  disabled={addableAgents.length === 0}
                  placeholder={addableAgents.length === 0 ? t("No agents available") : t("+ Add shared agent…")}
                  aria-label={t("Add an agent")}
                  options={addableAgents.map((a) => ({ value: a.uid, label: `@${a.alias}` }))}
                />
              </div>
              <Button variant="secondary" size="sm" type="submit" disabled={addAgentPick === ""}>{t("Add")}</Button>
            </form>
          )}
        </section>
      )}

      <section className="threads-context-section" aria-label={t("Active Guardrails")}>
        <details className="threads-guardrails-card" open={brakePaused || undefined}>
          <summary className="threads-guardrails-summary">
            <span className="threads-context-title">{t("Active Guardrails")}</span>
            <span className="threads-guardrails-pill" data-paused={brakePaused ? "true" : undefined}>
              {brakePaused
                ? t("Paused")
                : agentChainLimit > 0
                  ? `${agentChainCount}/${agentChainLimit}`
                  : t("Active")}
            </span>
          </summary>
          <ul className="threads-guardrails" data-active-guardrails="">
            <li>
              <strong>{t("Membership ACL")}</strong>
              <span>{t("Only admitted members receive notifications or @mentions.")}</span>
            </li>
            <li>
              <strong>{t("Personal assistants")}</strong>
              <span>{t("Human @mentions notify bell inbox only; private assistants stay isolated.")}</span>
            </li>
            {room.kind === "dm" ? (
              <li>
                <strong>{t("1:1 DM isolation")}</strong>
                <span>{t("Shared agents cannot join direct messages.")}</span>
              </li>
            ) : (
              <li>
                <strong>{t("Human-Turn Brake")}</strong>
                <span>
                  {agentChainLimit <= 0
                    ? t("Inactive until a shared agent joins this channel.")
                    : agentChainCount >= agentChainLimit
                      ? t("Paused after consecutive agent turns until a human posts.")
                      : t("Agent hops: {count}/{limit} before human turn required.")
                          .replace("{count}", String(agentChainCount))
                          .replace("{limit}", String(agentChainLimit))}
                </span>
              </li>
            )}
          </ul>
        </details>
      </section>

      <PersonNote error={peopleError} />
      {error && <p className="threads-note" role="status">{error}</p>}

      {pendingRemove && (
        <ConfirmDialog
          title={t(pendingRemove.kind === "agent" ? "Remove agent" : "Remove member")}
          role="alertdialog"
          initialFocus="cancel"
          body={
            <div className="threads-remove-card">
              <div className="threads-remove-target">
                <MemberAvatar
                  kind={pendingRemove.kind}
                  label={pendingRemove.label}
                  look={lookForMember(pendingRemove)}
                  size={28}
                />
                <span className="threads-remove-target-copy">
                  <strong>{pendingRemove.kind === "agent" ? `@${cleanAgentAlias(pendingRemove.label)}` : pendingRemove.label}</strong>
                  <span className="threads-remove-role">
                    {t(pendingRemove.kind === "agent" ? "Shared agent" : "Teammate")}
                  </span>
                </span>
              </div>
              <p className="threads-note m-0">
                {t(
                  pendingRemove.kind === "agent"
                    ? "Removing @{name} stops future @mentions in this channel from queueing turns."
                    : "Removing {name} revokes access to future messages and earlier channel history before re-invite.",
                ).replace("{name}", () => pendingRemove.label)}
              </p>
            </div>
          }
          confirmLabel={t(removing ? "Removing…" : "Remove")}
          confirmVariant="danger"
          pending={removing}
          error={removeError}
          onConfirm={() => void confirmRemoveMember()}
          onCancel={() => {
            if (!removing) {
              setPendingRemove(null);
              setRemoveError(null);
            }
          }}
        />
      )}
    </div>
  );
}
