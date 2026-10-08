import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useQuery, useResource } from "../../lib/useResource";
import type { Agent } from "../../lib/types";
import { activityTimeMs } from "../../lib/fmt";
import { recentAgentUpdates, recentWorkingAgents, reportedAgentUpdates, type AgentUpdate } from "./agentUpdateModel";
import { ageLabel } from "./needs";

// Cap concurrent history reads across the rail. Thread tails are small, so a
// freed slot goes to a waiting thread before a waiting agent detail: notes
// render while the slow detail is still pending.
let pendingReads = 0;
const urgentWaiting: (() => void)[] = [];
const waiting: (() => void)[] = [];
async function historyRead<T>(read: () => Promise<T>, urgent: boolean): Promise<T> {
  if (pendingReads >= 3) await new Promise<void>((resolve) => (urgent ? urgentWaiting : waiting).push(resolve));
  else pendingReads++;
  try { return await read(); }
  finally {
    const next = urgentWaiting.shift() ?? waiting.shift();
    if (next) next(); else pendingReads--;
  }
}

/** One half of an agent's history: a settled read, never a rejected promise. */
type Part<T> = { ok: true; value: T } | { ok: false };
interface HistoryEntry { stamp: string; parts: Map<"detail" | "thread", Promise<Part<unknown>>> }
// Bounded: the 24 most recently changed agents keep their reads.
const HISTORY_LIMIT = 24;
const historyCache = new Map<string, HistoryEntry>();

/**
 * One read per agent part for an activity stamp. A success is reused until the
 * stamp changes. A failure is dropped as soon as it settles, so the next load
 * or Retry reads again instead of replaying the error.
 */
function cachedPart<T>(alias: string, stamp: string, key: "detail" | "thread", read: () => Promise<T>): Promise<Part<T>> {
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
    (value): Part<T> => ({ ok: true, value }),
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
  const context = agent.tasks?.find((t) => t.title || t.job_title)?.title ?? agent.tasks?.find((t) => t.job_title)?.job_title;
  return <li className="agent-update-card">
    <div className="flex items-center gap-2 min-w-0">
      <span className={`agent-presence ${agent.running > 0 ? "is-working" : agent.fenced ? "is-fenced" : ""}`} aria-hidden />
      <h3 className="text-secondary font-semibold text-ink-100 break-all flex-1">{agent.alias}</h3>
      <span className={`text-micro ${agent.fenced ? "text-warn" : "text-ink-400"}`}>{state}</span>
    </div>
    <div className="flex flex-wrap gap-2 mt-1 text-micro text-ink-500">
      {agent.on.map((id) => <button key={id} className="lnk num" onClick={() => onOpenIssue(id)}>{id}</button>)}
      <UpdatedAt at={agent.last_activity ?? null} />
    </div>
    {context && <p className="agent-context">{context}</p>}
    {recent.length > 0 && <ul className="agent-update-notes">
      {recent.map((update) => <li key={update.key}>
        <div className="flex gap-2 items-baseline text-micro text-ink-500"><span className="text-ink-300">{update.label}</span><UpdatedAt at={update.at} /></div>
        {update.text && <details className="agent-update-detail"><summary>{update.text.replace(/[#*`]/g, "").split("\n").filter(Boolean).slice(0, 2).join(" ").slice(0, 220)}</summary><p>{update.text}</p></details>}
      </li>)}
    </ul>}
    {line && <p className={`${line.warn ? "text-micro text-warn" : "text-label text-ink-500"} ${recent.length > 0 ? "mt-2" : "mt-3"}`}>
      {line.text}
      {line.retry && <button type="button" className="lnk text-micro ml-2" onClick={() => setAttempt((n) => n + 1)}>Retry</button>}
    </p>}
    <button className="lnk text-label mt-3" onClick={() => onAsk(agent, recent[0])}>Ask Master about this <span aria-hidden>↗</span></button>
  </li>;
}

export default function AgentUpdates({ onAsk, onOpenIssue }: {
  onAsk: (agent: Agent, update?: AgentUpdate) => void; onOpenIssue: (id: string) => void;
}) {
  const state = useResource(resources.agents);
  const thread = useQuery(resources.masterThread);
  const reports = reportedAgentUpdates(thread.data?.entries ?? []);
  const agents = recentWorkingAgents((state.data?.agents ?? []).map((agent) => {
    const created = agent.message?.created;
    const latest = Math.max(activityTimeMs(agent.last_activity), activityTimeMs(reports.get(agent.alias)?.[0]?.at), activityTimeMs(created));
    return { ...agent, last_activity: latest > 0 ? new Date(latest).toISOString() : agent.last_activity };
  }));
  return <div>
    <p className="px-4 py-3 text-label text-ink-500 border-b border-ink-700">Recent work across all projects. Ask Master to check in or explain an update.</p>
    {!state.data && <p className="px-4 py-4 text-label text-ink-500">{state.status === "failed" ? `Agent updates unavailable — ${state.error}` : "Reading agent activity…"}</p>}
    {state.data && state.data.daemon === "unreachable" && <p className="px-4 py-3 text-label text-warn">Daemon unreachable. Showing the last available activity.</p>}
    {state.data && agents.length === 0 && <p className="px-4 py-6 text-label text-ink-400">No agent activity yet. Ask Master to plan the next job.</p>}
    <ul>{agents.map((agent) => <AgentCard key={agent.alias} agent={agent} reports={reports.get(agent.alias) ?? []} onAsk={onAsk} onOpenIssue={onOpenIssue} />)}</ul>
    {agents.length === 12 && <p className="px-4 py-3 text-micro text-ink-500">Showing the 12 most recently active agents. See Team overview for everyone.</p>}
  </div>;
}
