import { useEffect, useState } from "react";
import { api } from "../../lib/api";
import { resources } from "../../lib/resources";
import { useQuery, useResource } from "../../lib/useResource";
import type { Agent, AgentDetail } from "../../lib/types";
import { recentAgentUpdates, recentWorkingAgents, reportedAgentUpdates, type AgentUpdate } from "./agentUpdates";
import { ageLabel } from "./needs";

// Cap concurrent history reads across the rail; reuse in-flight loads on rerenders.
let pendingReads = 0;
const waiting: (() => void)[] = [];
async function historyRead<T>(read: () => Promise<T>): Promise<T> {
  if (pendingReads >= 3) await new Promise<void>((resolve) => waiting.push(resolve));
  else pendingReads++;
  try { return await read(); }
  finally { const next = waiting.shift(); if (next) next(); else pendingReads--; }
}
const historyCache = new Map<string, { stamp: string; read: Promise<PromiseSettledResult<unknown>[]> }>();

function UpdatedAt({ at }: { at: string | null }) {
  const stamp = Date.parse(at ?? "");
  if (!Number.isFinite(stamp)) return null;
  return <time dateTime={at!} title={new Date(stamp).toLocaleString()}>{ageLabel(Math.max(0, (Date.now() - stamp) / 1000))} ago</time>;
}

function AgentCard({ agent, reports, onAsk, onOpenIssue }: {
  agent: Agent; reports: AgentUpdate[]; onAsk: (agent: Agent, update?: AgentUpdate) => void; onOpenIssue: (id: string) => void;
}) {
  const [updates, setUpdates] = useState<AgentUpdate[]>([]);
  const [error, setError] = useState(false);
  const [loaded, setLoaded] = useState(false);
  useEffect(() => {
    let active = true;
    // Bounded tails; re-read only when this agent's activity watermark changes.
    const stamp = `${agent.event_cursor ?? ""}:${agent.last_activity ?? ""}:${agent.state}:${agent.running}:${agent.queued}`;
    let cached = historyCache.get(agent.alias);
    if (!cached || cached.stamp !== stamp) {
      cached = { stamp, read: Promise.allSettled([
        historyRead(() => api.agent(agent.alias)),
        historyRead(() => api.thread(agent.alias, { tail: true, limit: 16 })),
      ]) };
      historyCache.set(agent.alias, cached);
      // Cache only a small recent working set, including fulfilled/in-flight reads.
      if (historyCache.size > 24) historyCache.delete(historyCache.keys().next().value!);
    }
    cached.read.then(([detail, thread]) => {
      if (!active) return;
      setUpdates(recentAgentUpdates(detail.status === "fulfilled" ? detail.value as AgentDetail : null,
        thread.status === "fulfilled" ? (thread.value as { entries: unknown[] }).entries : []));
      setError(detail.status === "rejected" || thread.status === "rejected");
      setLoaded(true);
    });
    return () => { active = false; };
  }, [agent.alias, agent.event_cursor, agent.last_activity, agent.state, agent.running, agent.queued]);
  const recent = [...updates, ...reports].sort((a, b) => (Date.parse(b.at ?? "") || 0) - (Date.parse(a.at ?? "") || 0)).slice(0, 3);
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
    {recent.length > 0 ? <ul className="agent-update-notes">
      {recent.map((update) => <li key={update.key}>
        <div className="flex gap-2 items-baseline text-micro text-ink-500"><span className="text-ink-300">{update.label}</span><UpdatedAt at={update.at} /></div>
        {update.text && <details className="agent-update-detail"><summary>{update.text.replace(/[#*`]/g, "").split("\n").filter(Boolean).slice(0, 2).join(" ").slice(0, 220)}</summary><p>{update.text}</p></details>}
      </li>)}
    </ul> : <p className="text-label text-ink-500 mt-3">{loaded ? "No recent progress notes." : "Reading recent updates…"}</p>}
    {error && <p className="text-micro text-warn mt-2">Some recent updates could not be loaded.</p>}
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
    const started = typeof created === "number" ? created * 1000 : Date.parse(created ?? "");
    const latest = Math.max(Date.parse(agent.last_activity ?? "") || 0, Date.parse(reports.get(agent.alias)?.[0]?.at ?? "") || 0, Number.isFinite(started) ? started : 0);
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
