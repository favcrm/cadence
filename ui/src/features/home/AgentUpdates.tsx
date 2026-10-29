import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import { resources } from "../../lib/resources";
import { onPrivateReadsPaused, onPrivateSessionReset, privateReadsAllowed, privateSessionGeneration } from "../../lib/privateSession";
import { useQuery, useResource } from "../../lib/useResource";
import type { Agent } from "../../lib/types";
import { activityTimeMs } from "../../lib/fmt";
import { useTranslate } from "../../lib/locale";
import Link from "../../ui/Link";
import AgentAvatar, { agentLook } from "../agents/AgentAvatar";
import { activeTasks, agentThreadHref } from "../agents/AgentBlocks";
import { activeAssignedGroups, agentWorkHref, recentAgentUpdates, recentWorkingAgents, reportedAgentUpdates, updatesNotices, type AgentUpdate } from "./agentUpdateModel";
import { ageLabel } from "./needs";
import { agentOn } from "../../lib/agentOn";

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

function statusLine(parts: Source[], noteCount: number): { text: string; warn: boolean; retry: boolean } | null {
  if (parts.some((p) => p.failed)) {
    if (noteCount === 0 && !parts.some((p) => p.loaded)) return { text: "Recent updates could not be loaded.", warn: true, retry: true };
    if (parts.some((p) => p.failed && !p.loaded)) return { text: "Some recent updates could not be loaded.", warn: true, retry: true };
    return { text: noteCount > 0 ? "Refresh failed; showing earlier updates." : "Refresh failed.", warn: true, retry: true };
  }
  // A refresh of notes already on screen says so; earlier notes stay visible.
  if (parts.some((p) => p.pending && p.loaded)) return { text: noteCount > 0 ? "Showing earlier updates while refreshing…" : "Refreshing recent updates…", warn: false, retry: false };
  if (parts.every((p) => p.loaded)) return noteCount === 0 ? { text: "No recent progress notes.", warn: false, retry: false } : null;
  return { text: noteCount === 0 ? "Reading recent updates…" : "Reading more updates…", warn: false, retry: false };
}

function UpdatedAt({ at }: { at: string | number | null }) {
  const stamp = activityTimeMs(at);
  if (!stamp) return null;
  const date = new Date(stamp);
  return <time dateTime={date.toISOString()} title={date.toLocaleString()}>{ageLabel(Math.max(0, (Date.now() - stamp) / 1000))} ago</time>;
}

function AgentCard({ agent, reports, onAsk, onOpenIssue }: {
  agent: Agent; reports: AgentUpdate[]; onAsk: (agent: Agent, update?: AgentUpdate) => void; onOpenIssue: (id: string) => void;
}) {
  const t = useTranslate();
  const [detail, setDetail] = useState<Source>(reading);
  const [thread, setThread] = useState<Source>(reading);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let active = true;
    // Re-read only when this agent's activity watermark changes, or on Retry.
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
  }, [agent.alias, agent.event_cursor, agent.last_activity, agent.state, agent.running, agent.queued, attempt]);
  const recent = [...detail.updates, ...thread.updates, ...reports].sort((a, b) => (Date.parse(b.at ?? "") || 0) - (Date.parse(a.at ?? "") || 0)).slice(0, 3);
  const line = statusLine([detail, thread], recent.length);
  const state = agent.fenced ? "Needs attention" : agent.running > 0 ? "Working" : agent.queued > 0 ? "Queued" : agent.state_label ?? agent.state;
  const active = activeTasks(agent);
  const context = active.find((task) => task.title || task.job_title)?.title ?? active.find((task) => task.job_title)?.job_title;
  return <li className="agent-update-card">
    <div className="flex items-center gap-2 min-w-0">
      <span className={`agent-presence ${agent.running > 0 ? "is-working" : agent.fenced ? "is-fenced" : ""}`} aria-hidden />
      <h3 className="text-secondary font-semibold text-ink-100 break-all flex-1">{agent.alias}</h3>
      <span className={`text-micro ${agent.fenced ? "text-warn" : "text-ink-400"}`}>{state}</span>
    </div>
    <div className="flex flex-wrap gap-2 mt-1 text-micro text-ink-500">
      {agentOn(agent).map((id) => <button key={id} className="lnk num" onClick={() => onOpenIssue(id)}>{id}</button>)}
      <UpdatedAt at={agent.last_activity ?? null} />
    </div>
    {context && <p className="agent-context">{context}</p>}
    {recent.length > 0 && <ul className="agent-update-notes">
      {recent.map((update) => {
        const issue = update.issue;
        return <li key={update.key}>
        <div className="flex gap-2 items-baseline text-micro text-ink-500"><span className="text-ink-300">{update.label}</span>
          {/* A report's stored issue id opens its real page; absent, the
              label stays the only read — never an invented link. */}
          {issue !== undefined && <button type="button" className="lnk num" onClick={() => onOpenIssue(issue)}>{issue}</button>}
          <UpdatedAt at={update.at} /></div>
        {update.text && <details className="agent-update-detail"><summary>{update.text.replace(/[#*`]/g, "").split("\n").filter(Boolean).slice(0, 2).join(" ").slice(0, 220)}</summary><p>{update.text}</p></details>}
      </li>;
      })}
    </ul>}
    {line && <p className={`${line.warn ? "text-micro text-warn" : "text-label text-ink-500"} ${recent.length > 0 ? "mt-2" : "mt-3"}`}>
      {t(line.text)}
      {line.retry && <button type="button" className="lnk text-micro ml-2" onClick={() => setAttempt((n) => n + 1)}>{t("Retry")}</button>}
    </p>}
    <button className="lnk text-label mt-3" onClick={() => onAsk(agent, recent[0])}>{t("Ask the Assistant about this")} <span aria-hidden>↗</span></button>
  </li>;
}

function ActiveAssigned({ agents }: { agents: Agent[] }) {
  const t = useTranslate();
  const groups = activeAssignedGroups(agents);
  if (groups.length === 0) return null;
  return (
    <section aria-label={t("Active assigned work")} data-active-work>
      <ul>
        {groups.map((group) => {
          const look = typeof group.agent.look === "string" ? group.agent.look : undefined;
          return (
            <li key={group.alias} className="agent-update-card" data-agent={group.alias}>
              <Link href={agentWorkHref(group.alias)} className="flex items-center gap-2 min-w-0">
                <AgentAvatar
                  slug={agentLook(group.alias, look ? { look } : group.agent.params)}
                  look={look}
                  size={28}
                  still={group.agent.running === 0}
                  active={group.agent.running > 0}
                />
                <span className="text-secondary font-semibold text-ink-100 break-all">{group.alias}</span>
              </Link>
              <ul className="agent-update-notes">
                {group.tasks.map((task) => (
                  <li key={task.task}>
                    <Link className="lnk" href={agentThreadHref(group.alias)} data-task={task.task}>
                      {task.title ?? task.task}
                    </Link>
                    <div className="text-micro text-ink-500">
                      {task.task_state}
                      {task.job_title ? ` · ${task.job_title}` : ""}
                    </div>
                  </li>
                ))}
              </ul>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

export default function AgentUpdates({ onAsk, onOpenIssue }: {
  onAsk: (agent: Agent, update?: AgentUpdate) => void; onOpenIssue: (id: string) => void;
}) {
  const t = useTranslate();
  const state = useResource(resources.agents);
  const thread = useQuery(resources.masterThread);
  const reports = reportedAgentUpdates(thread.data?.entries ?? []);
  const roster = state.data?.agents ?? [];
  const agents = recentWorkingAgents(roster.map((agent) => {
    const created = agent.message?.created;
    const latest = Math.max(activityTimeMs(agent.last_activity), activityTimeMs(reports.get(agent.alias)?.[0]?.at), activityTimeMs(created));
    return { ...agent, last_activity: latest > 0 ? new Date(latest).toISOString() : agent.last_activity };
  }));
  const notices = updatesNotices({
    hasData: Boolean(state.data),
    daemon: state.data?.daemon,
    refreshError: Boolean(state.data?.refresh_error),
    recentCount: agents.length,
    resourceStatus: state.status,
  });
  return <div>
    <p className="px-4 py-3 text-label text-ink-500 border-b border-ink-700">{t("Recent work across all projects. Ask the Assistant to check in or explain an update.")}</p>
    {!state.data && <p className="px-4 py-4 text-label text-ink-500">{state.status === "failed" ? `${t("Agent updates unavailable")} — ${state.error}` : t("Reading agent activity…")}</p>}
    {notices.unreachable && <p className="px-4 py-3 text-label text-warn">{t("Daemon unreachable. Showing the last available activity.")}</p>}
    {notices.partial && <p className="px-4 py-3 text-label text-warn">{t("Some agent records could not be read. Other active work may be missing.")}</p>}
    {state.data && <ActiveAssigned agents={roster} />}
    {notices.empty && <p className="px-4 py-6 text-label text-ink-400">{t("No agent activity yet. Ask the Assistant to plan the next job.")}</p>}
    {notices.unavailable && <p className="px-4 py-6 text-label text-warn">{t("Active assigned work could not be read.")}</p>}
    <ul>{agents.map((agent) => <AgentCard key={agent.alias} agent={agent} reports={reports.get(agent.alias) ?? []} onAsk={onAsk} onOpenIssue={onOpenIssue} />)}</ul>
    {agents.length === 12 && <p className="px-4 py-3 text-micro text-ink-500">{t("Showing the 12 most recently active agents. See Team overview for everyone.")}</p>}
  </div>;
}
