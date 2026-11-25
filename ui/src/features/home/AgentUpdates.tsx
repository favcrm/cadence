import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../../lib/api";
import { resources } from "../../lib/resources";
import { onPrivateReadsPaused, onPrivateSessionReset, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import { useMaybeResource, useResource } from "../../lib/useResource";
import type { Agent, AgentFeedItem } from "../../lib/types";
import { activityTimeMs } from "../../lib/fmt";
import { useTranslate } from "../../lib/locale";
import Link from "../../ui/Link";
import { activeTasks } from "../agents/AgentBlocks";
import { activeAssignedGroups, recentAgentUpdates, recentWorkingAgents, reportedAgentUpdates, updatesNotices, type AgentUpdate } from "./agentUpdateModel";
import { FeedItemCard } from "./FeedItemCard";
import {
  buildFallbackFeedItems,
  feedBucketForState,
  feedCategoriesForItem,
  feedItemMatchesFilter,
  isLiveFeedState,
  mergeFeedItems,
  type FeedBucket,
} from "./feedRailModel";
import type { RailFeedTarget } from "./RailFeedDrawer";
import {
  countFeedCategories,
  feedCategoriesForAgent,
  type FeedCategory,
} from "./railModel";

// Cap concurrent history reads across the rail. Thread tails are small, so a
// freed slot goes to a waiting thread before a waiting agent detail: notes
// render while the slow detail is still pending.
// Queued reads have no API operation yet; pause must retire their lease too.
let historyEpoch = 0;
let pendingReads = 0;
const urgentWaiting: (() => void)[] = [];
const waiting: (() => void)[] = [];
async function historyRead<T>(read: () => Promise<T>, urgent: boolean): Promise<T> {
  const generation = privateSessionGeneration();
  const epoch = historyEpoch;
  if (pendingReads >= 3) await new Promise<void>((resolve) => (urgent ? urgentWaiting : waiting).push(resolve));
  else pendingReads++;
  try {
    if (epoch !== historyEpoch || generation !== privateSessionGeneration() || !privateReadsAllowed()) {
      throw new DOMException("The private session changed", "AbortError");
    }
    return await read();
  } finally {
    if (generation === privateSessionGeneration()) {
      const next = urgentWaiting.shift() ?? waiting.shift();
      if (next) next(); else pendingReads--;
    }
  }
}

/** One half of an agent's history: a settled read, never a rejected promise. */
type Part<T> = { ok: true; value: T } | { ok: false };
interface HistoryEntry { stamp: string; parts: Map<"detail" | "thread", Promise<Part<unknown>>> }
// Bounded: the 24 most recently changed agents keep their reads.
const HISTORY_LIMIT = 24;
const historyCache = new Map<string, HistoryEntry>();

onPrivateReadsPaused(() => {
  historyEpoch += 1;
  historyCache.clear();
});

onPrivateSessionReset(() => {
  pendingReads = 0;
  // Release queued jobs only so they can observe their expired generation;
  // they must not start a read under the next identity.
  for (const resume of urgentWaiting.splice(0)) resume();
  for (const resume of waiting.splice(0)) resume();
});

/**
 * One read per agent part for an activity stamp. A success is reused until the
 * stamp changes. A failure is dropped as soon as it settles, so the next load
 * or Retry reads again instead of replaying the error.
 */
function cachedPart<T>(alias: string, stamp: string, key: "detail" | "thread", read: () => Promise<T>): Promise<Part<T>> {
  const generation = privateSessionGeneration();
  const epoch = historyEpoch;
  if (!privateReadsAllowed()) return Promise.resolve({ ok: false });
  let entry = historyCache.get(alias);
  if (!entry || entry.stamp !== stamp) {
    entry = { stamp, parts: new Map() };
    historyCache.delete(alias);
    historyCache.set(alias, entry);
    while (historyCache.size > HISTORY_LIMIT) historyCache.delete(historyCache.keys().next().value!);
  }
  const owner = entry;
  const known = owner.parts.get(key) as Promise<Part<T>> | undefined;
  if (known) return known;
  const attempt: Promise<Part<T>> = historyRead(read, key === "thread").then(
    (value): Part<T> => {
      if (epoch === historyEpoch && generation === privateSessionGeneration() && privateReadsAllowed()) return { ok: true, value };
      if (owner.parts.get(key) === attempt) owner.parts.delete(key);
      return { ok: false };
    },
    (): Part<T> => {
      if (owner.parts.get(key) === attempt) owner.parts.delete(key);
      return { ok: false };
    },
  );
  owner.parts.set(key, attempt);
  return attempt;
}

/** What one part of a card holds: notes from its last success, and whether a read is running. */
interface Source { updates: AgentUpdate[]; loaded: boolean; pending: boolean; failed: boolean }
const reading: Source = { updates: [], loaded: false, pending: true, failed: false };

export interface AgentFeedState {
  counts: Record<FeedCategory, number>;
  liveCount: number;
  hasAny: boolean;
}

const BUCKET_LABELS: Record<FeedBucket, string> = {
  attention: "Needs your attention",
  live: "Live now",
  recent: "Recent",
};

/**
 * Fallback history reader for native/standalone agents when `/api/agents`
 * omits pre-projected `feed_items` (or leaves an active agent unprojected).
 * Preserves per-agent `cachedPart` pause/cancellation semantics.
 */
function FallbackAgentHistoryReader({
  agent,
  onLoaded,
}: {
  agent: Agent;
  onLoaded: (alias: string, updates: AgentUpdate[]) => void;
}) {
  const [detail, setDetail] = useState<Source>(reading);
  const [thread, setThread] = useState<Source>(reading);

  useEffect(() => {
    let active = true;
    const stamp = `${agent.event_cursor ?? ""}:${agent.last_activity ?? ""}:${agent.state}:${agent.running}:${agent.queued}`;
    setDetail((previous) => ({ ...previous, pending: true, failed: false }));
    setThread((previous) => ({ ...previous, pending: true, failed: false }));
    void cachedPart(agent.alias, stamp, "thread", () => api.thread(agent.alias, { tail: true, limit: 16 })).then((part) => {
      if (!active) return;
      setThread((previous) => (part.ok
        ? { updates: recentAgentUpdates(null, part.value.entries), loaded: true, pending: false, failed: false }
        : { ...previous, pending: false, failed: true }));
    });
    void cachedPart(agent.alias, stamp, "detail", () => api.agent(agent.alias)).then((part) => {
      if (!active) return;
      setDetail((previous) => (part.ok
        ? { updates: recentAgentUpdates(part.value, []), loaded: true, pending: false, failed: false }
        : { ...previous, pending: false, failed: true }));
    });
    return () => {
      active = false;
    };
  }, [agent.alias, agent.event_cursor, agent.last_activity, agent.state, agent.running, agent.queued]);

  useEffect(() => {
    const merged = [...detail.updates, ...thread.updates]
      .sort((a, b) => (Date.parse(b.at ?? "") || 0) - (Date.parse(a.at ?? "") || 0))
      .slice(0, 4);
    onLoaded(agent.alias, merged);
  }, [agent.alias, detail.updates, thread.updates, onLoaded]);

  return null;
}

export default function AgentUpdates({
  onAsk,
  onAskFeedItem,
  category = "all",
  includeMasterReports = true,
  onOpenAgent,
  onOpenFeedItem,
  onAgentFeedState,
}: {
  onAsk?: (agent: Agent, update?: AgentUpdate) => void;
  onAskFeedItem?: (item: AgentFeedItem) => void;
  onOpenIssue?: (id: string) => void;
  category?: FeedCategory;
  includeMasterReports?: boolean;
  onOpenAgent?: (target: Extract<RailFeedTarget, { kind: "agent" }>, opener: HTMLElement) => void;
  onOpenFeedItem?: (item: AgentFeedItem, opener: HTMLElement) => void;
  onAgentFeedState?: (state: AgentFeedState) => void;
}) {
  const t = useTranslate();
  const state = useResource(resources.agents);
  const masterThread = useMaybeResource(includeMasterReports ? resources.masterThread : null);
  useEffect(() => {
    if (includeMasterReports) void resources.masterThread.revalidate();
  }, [includeMasterReports]);

  const reports = useMemo(
    () => (includeMasterReports ? reportedAgentUpdates(masterThread?.data?.entries ?? []) : new Map<string, AgentUpdate[]>()),
    [includeMasterReports, masterThread?.data?.entries],
  );
  const roster = state.data?.agents ?? [];
  const agents = useMemo(() => {
    const groups = activeAssignedGroups(roster);
    const enriched = roster.map((agent) => {
      const created = agent.message?.created;
      const latest = Math.max(
        activityTimeMs(agent.last_activity),
        activityTimeMs(reports.get(agent.alias)?.[0]?.at),
        activityTimeMs(created),
      );
      return latest > 0 ? { ...agent, last_activity: new Date(latest).toISOString() } : agent;
    });
    const recent = recentWorkingAgents(enriched);
    const seen = new Set(recent.map((a) => a.alias));
    const missingActive = groups
      .filter((g) => !seen.has(g.alias))
      .map((g) => enriched.find((a) => a.alias === g.alias) ?? g.agent);
    return [...recent, ...missingActive].slice(0, 12);
  }, [roster, reports]);

  const serverItems = state.data?.feed_items;
  const serverAliases = useMemo(
    () => new Set((serverItems ?? []).map((item) => item.alias.toLowerCase())),
    [serverItems],
  );
  const fallbackAgents = useMemo(
    () => agents.filter((agent) => !serverAliases.has(agent.alias.toLowerCase())),
    [agents, serverAliases],
  );

  const [updatesByAlias, setUpdatesByAlias] = useState<Map<string, AgentUpdate[]>>(() => new Map());
  const onHistoryLoaded = useCallback((alias: string, updates: AgentUpdate[]) => {
    setUpdatesByAlias((prev) => {
      const existing = prev.get(alias);
      const nextSig = updates.map((u) => `${u.key}:${u.at ?? ""}`).join("|");
      const prevSig = (existing ?? []).map((u) => `${u.key}:${u.at ?? ""}`).join("|");
      if (existing !== undefined && nextSig === prevSig) return prev;
      const next = new Map(prev);
      next.set(alias, updates);
      return next;
    });
  }, []);

  const fallbackItems = useMemo(
    () => buildFallbackFeedItems({ agents: fallbackAgents, reportsByAlias: reports, updatesByAlias }),
    [fallbackAgents, reports, updatesByAlias],
  );

  const feedItems = useMemo(
    () => mergeFeedItems(serverItems, fallbackItems),
    [serverItems, fallbackItems],
  );

  // Brief subtle delta pulse when an existing item changes status or latest_action.
  const prevItemStamps = useRef<Map<string, string>>(new Map());
  const seededStamps = useRef(false);
  const [justUpdatedIds, setJustUpdatedIds] = useState<Set<string>>(() => new Set());
  useEffect(() => {
    if (feedItems.length === 0) return;
    if (!seededStamps.current) {
      seededStamps.current = true;
      for (const item of feedItems) {
        prevItemStamps.current.set(item.id, `${item.state}:${item.latest_action}:${item.updated_at}`);
      }
      return;
    }
    const changed: string[] = [];
    for (const item of feedItems) {
      const stamp = `${item.state}:${item.latest_action}:${item.updated_at}`;
      const prior = prevItemStamps.current.get(item.id);
      if (prior !== undefined && prior !== stamp) changed.push(item.id);
      prevItemStamps.current.set(item.id, stamp);
    }
    if (changed.length === 0) return;
    setJustUpdatedIds((prev) => {
      const next = new Set(prev);
      for (const id of changed) next.add(id);
      return next;
    });
    const timer = window.setTimeout(() => {
      setJustUpdatedIds((prev) => {
        const next = new Set(prev);
        for (const id of changed) next.delete(id);
        return next;
      });
    }, 2200);
    return () => window.clearTimeout(timer);
  }, [feedItems]);

  const feedSummary = useMemo<AgentFeedState>(() => {
    const entries = feedItems.map((item) => ({
      categories: feedCategoriesForItem(item),
      live: isLiveFeedState(item.state),
    }));
    // Keep category counts consistent for agents lacking feed_items metadata too.
    if (entries.length === 0 && agents.length > 0) {
      const legacy = agents.map((agent) => ({
        categories: feedCategoriesForAgent(agent, activeTasks(agent).length > 0, reports.get(agent.alias) ?? []),
        live: agent.running > 0,
      }));
      return {
        counts: countFeedCategories(legacy),
        liveCount: legacy.filter((e) => e.live).length,
        hasAny: legacy.length > 0,
      };
    }
    return {
      counts: countFeedCategories(entries),
      liveCount: entries.filter((e) => e.live).length,
      hasAny: entries.length > 0,
    };
  }, [feedItems, agents, reports]);

  const lastEmitted = useRef("");
  useEffect(() => {
    if (!onAgentFeedState) return;
    const sig = JSON.stringify(feedSummary);
    if (sig === lastEmitted.current) return;
    lastEmitted.current = sig;
    onAgentFeedState(feedSummary);
  }, [feedSummary, onAgentFeedState]);

  const visibleItems = useMemo(
    () => feedItems.filter((item) => feedItemMatchesFilter(item, category)),
    [feedItems, category],
  );

  const visibleBuckets = useMemo(() => {
    const counts = new Set<FeedBucket>();
    for (const item of visibleItems) counts.add(feedBucketForState(item.state));
    return counts.size;
  }, [visibleItems]);

  const agentByAlias = useMemo(() => {
    const map = new Map<string, Agent>();
    for (const agent of roster) map.set(agent.alias.toLowerCase(), agent);
    return map;
  }, [roster]);

  const notices = updatesNotices({
    hasData: Boolean(state.data),
    daemon: state.data?.daemon,
    refreshError: Boolean(state.data?.refresh_error),
    recentCount: feedItems.length || agents.length,
    resourceStatus: state.status,
  });

  const handleAsk = useCallback((item: AgentFeedItem) => {
    if (onAskFeedItem) {
      onAskFeedItem(item);
      return;
    }
    const agent = agentByAlias.get(item.alias.toLowerCase());
    if (agent && onAsk) {
      onAsk(agent, {
        key: item.id,
        label: item.action_verb,
        text: item.latest_action,
        at: item.updated_at,
        issue: item.ticket_id ?? undefined,
      });
    }
  }, [onAskFeedItem, agentByAlias, onAsk]);

  const handleOpenDetails = useCallback((item: AgentFeedItem, opener: HTMLElement) => {
    if (onOpenFeedItem) {
      onOpenFeedItem(item, opener);
      return;
    }
    const agent = agentByAlias.get(item.alias.toLowerCase());
    if (agent && onOpenAgent) {
      onOpenAgent(
        {
          kind: "agent",
          agent,
          updates: updatesByAlias.get(agent.alias) ?? reports.get(agent.alias) ?? [],
          tasks: activeTasks(agent),
          stateLabel: item.state_label,
        },
        opener,
      );
      return;
    }
    handleAsk(item);
  }, [onOpenFeedItem, agentByAlias, onOpenAgent, updatesByAlias, reports, handleAsk]);

  let lastBucket: FeedBucket | null = null;

  return (
    <>
      {fallbackAgents.map((agent) => (
        <FallbackAgentHistoryReader key={agent.alias} agent={agent} onLoaded={onHistoryLoaded} />
      ))}
      {!state.data && (
        <p className="rail-state-card">
          {state.status === "failed" ? `${t("Agent updates unavailable")} — ${state.error}` : t("Reading agent activity…")}
        </p>
      )}
      {notices.unreachable && (
        <p className="rail-state-card warn">{t("Daemon unreachable. Showing the last available activity.")}</p>
      )}
      {notices.partial && (
        <p className="rail-state-card warn">{t("Some agent records could not be read. Other active work may be missing.")}</p>
      )}
      {notices.unavailable && (
        <p className="rail-state-card warn">{t("Active assigned work could not be read.")}</p>
      )}
      {notices.empty && !onOpenAgent && !onOpenFeedItem && (
        <p className="rail-state-card">{t("No agent activity yet. Ask the Assistant to plan the next job.")}</p>
      )}
      {feedItems.length > 0 && (
        <ul className="rail-cards" hidden={visibleItems.length === 0}>
          {visibleItems.map((item) => {
            const bucket = feedBucketForState(item.state);
            const showHeader = category === "all" && visibleBuckets > 1 && bucket !== lastBucket;
            lastBucket = bucket;
            return (
              <FragmentRow
                key={item.id}
                header={showHeader ? t(BUCKET_LABELS[bucket]) : null}
                item={item}
                justUpdated={justUpdatedIds.has(item.id)}
                onAsk={onAskFeedItem || onAsk ? handleAsk : undefined}
                onOpenDetails={handleOpenDetails}
              />
            );
          })}
        </ul>
      )}
      {category === "all" && agents.length === 12 && (
        <p className="px-3 pb-3 text-micro text-ink-500">
          {t("Showing the 12 most recently active agents.")}{" "}
          <Link href="/agents" className="lnk">{t("See all agents")}</Link>
        </p>
      )}
    </>
  );
}

function FragmentRow({
  header,
  item,
  justUpdated,
  onAsk,
  onOpenDetails,
}: {
  header: string | null;
  item: AgentFeedItem;
  justUpdated: boolean;
  onAsk?: (item: AgentFeedItem) => void;
  onOpenDetails?: (item: AgentFeedItem, opener: HTMLElement) => void;
}) {
  return (
    <>
      {header ? (
        <li className="rail-sec-li" aria-hidden="true">
          <div className="rail-sec">{header}</div>
        </li>
      ) : null}
      <FeedItemCard
        item={item}
        justUpdated={justUpdated}
        onAsk={onAsk}
        onOpenDetails={onOpenDetails}
      />
    </>
  );
}
