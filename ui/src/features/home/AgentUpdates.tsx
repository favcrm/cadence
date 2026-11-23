import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../../lib/api";
import { resources } from "../../lib/resources";
import { onPrivateReadsPaused, onPrivateSessionReset, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import { useMaybeResource, useResource } from "../../lib/useResource";
import type { Agent } from "../../lib/types";
import { activityTimeMs } from "../../lib/fmt";
import { useTranslate } from "../../lib/locale";
import Link from "../../ui/Link";
import { activeTasks } from "../agents/AgentBlocks";
import { activeAssignedGroups, recentAgentUpdates, recentWorkingAgents, reportedAgentUpdates, updatesNotices, type AgentUpdate } from "./agentUpdateModel";
import { ageLabel } from "./needs";
import { agentOn } from "../../lib/agentOn";
import { RailCard } from "./RailCard";
import type { RailFeedTarget } from "./RailFeedDrawer";
import {
  countFeedCategories,
  feedCategoriesForAgent,
  type FeedCategory,
  type FeedCategoryKey,
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

function statusLine(parts: Source[], noteCount: number): { text: string; warn: boolean } | null {
  if (parts.some((p) => p.failed)) {
    if (noteCount === 0 && !parts.some((p) => p.loaded)) return { text: "Recent updates could not be loaded.", warn: true };
    if (parts.some((p) => p.failed && !p.loaded)) return { text: "Some recent updates could not be loaded.", warn: true };
    return { text: noteCount > 0 ? "Refresh failed; showing earlier updates." : "Refresh failed.", warn: true };
  }
  // A refresh of notes already on screen says so; earlier notes stay visible.
  if (parts.some((p) => p.pending && p.loaded)) return { text: noteCount > 0 ? "Showing earlier updates while refreshing…" : "Refreshing recent updates…", warn: false };
  if (parts.every((p) => p.loaded)) return noteCount === 0 ? { text: "No recent progress notes.", warn: false } : null;
  return { text: noteCount === 0 ? "Reading recent updates…" : "Reading more updates…", warn: false };
}

function initialsFor(alias: string): string {
  const clean = alias.replace(/[^a-zA-Z0-9]/g, "");
  return (clean.slice(0, 2) || "AG").toUpperCase();
}

function summarizeNote(text: string): string {
  return text.replace(/[#*`]/g, "").split("\n").filter(Boolean).slice(0, 2).join(" ").slice(0, 220);
}

export interface AgentFeedState {
  counts: Record<FeedCategory, number>;
  liveCount: number;
  hasAny: boolean;
}

function AgentCard({
  agent,
  reports,
  category,
  onAsk,
  onOpenAgent,
  onMeta,
}: {
  agent: Agent;
  reports: AgentUpdate[];
  category: FeedCategory;
  onAsk?: (agent: Agent, update?: AgentUpdate) => void;
  onOpenAgent?: (target: Extract<RailFeedTarget, { kind: "agent" }>, opener: HTMLElement) => void;
  onMeta?: (alias: string, meta: { categories: FeedCategoryKey[]; live: boolean }) => void;
}) {
  const t = useTranslate();
  const [detail, setDetail] = useState<Source>(reading);
  const [thread, setThread] = useState<Source>(reading);
  useEffect(() => {
    let active = true;
    // Re-read only when this agent's activity watermark changes.
    const stamp = `${agent.event_cursor ?? ""}:${agent.last_activity ?? ""}:${agent.state}:${agent.running}:${agent.queued}`;
    // Earlier notes stay on screen while the read runs.
    setDetail((previous) => ({ ...previous, pending: true, failed: false }));
    setThread((previous) => ({ ...previous, pending: true, failed: false }));
    // Each part settles on its own: a slow or failed detail never holds back the notes.
    void cachedPart(agent.alias, stamp, "thread", () => api.thread(agent.alias, { tail: true, limit: 16 })).then((part) => {
      if (!active) return;
      setThread((previous) => part.ok
        ? { updates: recentAgentUpdates(null, part.value.entries), loaded: true, pending: false, failed: false }
        : { ...previous, pending: false, failed: true });
    });
    void cachedPart(agent.alias, stamp, "detail", () => api.agent(agent.alias)).then((part) => {
      if (!active) return;
      setDetail((previous) => part.ok
        ? { updates: recentAgentUpdates(part.value, []), loaded: true, pending: false, failed: false }
        : { ...previous, pending: false, failed: true });
    });
    return () => { active = false; };
  }, [agent.alias, agent.event_cursor, agent.last_activity, agent.state, agent.running, agent.queued]);

  const recent = useMemo(
    () =>
      [...detail.updates, ...thread.updates, ...reports]
        .sort((a, b) => (Date.parse(b.at ?? "") || 0) - (Date.parse(a.at ?? "") || 0))
        .slice(0, 3),
    [detail.updates, thread.updates, reports],
  );
  const line = statusLine([detail, thread], recent.length);
  const state = agent.fenced
    ? "Needs attention"
    : agent.running > 0
      ? "Working"
      : agent.queued > 0
        ? "Queued"
        : (agent.state_label ?? agent.state ?? "Idle");
  const active = activeTasks(agent);
  const categories = useMemo(
    () => feedCategoriesForAgent(agent, active.length > 0, recent),
    [agent, active.length, recent],
  );
  const live = agent.running > 0;
  useEffect(() => {
    onMeta?.(agent.alias, { categories, live });
  }, [agent.alias, categories, live, onMeta]);

  if (category !== "all" && !categories.includes(category)) return null;

  const context =
    active.find((task) => task.title || task.job_title)?.title ??
    active.find((task) => task.job_title)?.job_title;
  const latestUpdate = recent[0];
  const latestStamp = Math.max(
    activityTimeMs(latestUpdate?.at ?? null),
    activityTimeMs(agent.last_activity ?? null),
  );
  const age = latestStamp > 0
    ? `${ageLabel(Math.max(0, (Date.now() - latestStamp) / 1000))} ${t("ago")}`
    : undefined;
  const tone = agent.fenced ? "warn" : live ? "ok" : "info";
  const headline = context ?? (latestUpdate
    ? (latestUpdate.label === "Progress update" && latestUpdate.text
      ? summarizeNote(latestUpdate.text)
      : t(latestUpdate.label))
    : t(state));
  const usedNoteAsHeadline = !context && latestUpdate?.label === "Progress update" && Boolean(latestUpdate.text);
  const summary = usedNoteAsHeadline
    ? undefined
    : latestUpdate?.text
      ? summarizeNote(latestUpdate.text)
      : line
        ? t(line.text)
        : undefined;
  const issues = agentOn(agent);
  const meta = issues.length > 0
    ? issues.join(" · ")
    : latestUpdate?.issue
      ? latestUpdate.issue
      : active.length > 0
        ? `${active.length} ${t(active.length === 1 ? "task" : "tasks")}`
        : undefined;

  return (
    <li className="needrow" data-agent={agent.alias}>
      <RailCard
        tone={tone}
        live={live}
        statusLabel={t(state)}
        initials={initialsFor(agent.alias)}
        source={agent.alias}
        age={age}
        title={headline}
        summary={summary}
        meta={meta}
        cue={t("Details")}
        onOpen={(event) => {
          if (onOpenAgent) {
            onOpenAgent(
              { kind: "agent", agent, updates: recent, tasks: active, stateLabel: state },
              event.currentTarget,
            );
          } else {
            onAsk?.(agent, latestUpdate);
          }
        }}
      />
    </li>
  );
}

export default function AgentUpdates({
  onAsk,
  category = "all",
  includeMasterReports = true,
  onOpenAgent,
  onAgentFeedState,
}: {
  onAsk?: (agent: Agent, update?: AgentUpdate) => void;
  onOpenIssue?: (id: string) => void;
  category?: FeedCategory;
  includeMasterReports?: boolean;
  onOpenAgent?: (target: Extract<RailFeedTarget, { kind: "agent" }>, opener: HTMLElement) => void;
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

  const [metaByAlias, setMetaByAlias] = useState<
    Record<string, { categories: FeedCategoryKey[]; live: boolean }>
  >({});
  const onMeta = useCallback((alias: string, meta: { categories: FeedCategoryKey[]; live: boolean }) => {
    setMetaByAlias((prev) => {
      const cur = prev[alias];
      if (cur && cur.live === meta.live && cur.categories.join(",") === meta.categories.join(",")) {
        return prev;
      }
      return { ...prev, [alias]: meta };
    });
  }, []);

  const feedSummary = useMemo<AgentFeedState>(() => {
    const entries = agents.map((agent) => {
      const cached = metaByAlias[agent.alias];
      if (cached) return cached;
      const tasks = activeTasks(agent);
      return {
        categories: feedCategoriesForAgent(agent, tasks.length > 0, reports.get(agent.alias) ?? []),
        live: agent.running > 0,
      };
    });
    return {
      counts: countFeedCategories(entries),
      liveCount: entries.filter((e) => e.live).length,
      hasAny: entries.length > 0,
    };
  }, [agents, metaByAlias, reports]);

  const lastEmitted = useRef("");
  useEffect(() => {
    if (!onAgentFeedState) return;
    const sig = JSON.stringify(feedSummary);
    if (sig === lastEmitted.current) return;
    lastEmitted.current = sig;
    onAgentFeedState(feedSummary);
  }, [feedSummary, onAgentFeedState]);

  const notices = updatesNotices({
    hasData: Boolean(state.data),
    daemon: state.data?.daemon,
    refreshError: Boolean(state.data?.refresh_error),
    recentCount: agents.length,
    resourceStatus: state.status,
  });

  const matchingCount = category === "all" ? agents.length : feedSummary.counts[category];

  return (
    <>
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
      {notices.empty && !onOpenAgent && (
        <p className="rail-state-card">{t("No agent activity yet. Ask the Assistant to plan the next job.")}</p>
      )}
      {agents.length > 0 && (
        <ul className="rail-cards" hidden={matchingCount === 0}>
          {agents.map((agent) => (
            <AgentCard
              key={agent.alias}
              agent={agent}
              reports={reports.get(agent.alias) ?? []}
              category={category}
              onAsk={onAsk}
              onOpenAgent={onOpenAgent}
              onMeta={onMeta}
            />
          ))}
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
