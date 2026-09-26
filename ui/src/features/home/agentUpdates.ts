import type { Agent, AgentDetail } from "../../lib/types";
import { asEntry } from "./thread";

export interface AgentUpdate {
  key: string;
  label: string;
  text: string;
  at: string | null;
}

/** Keep provider turn completion distinct from a verified job completion. */
export function recentAgentUpdates(detail: AgentDetail | null, entries: unknown[]): AgentUpdate[] {
  const events = (detail?.events ?? []).flatMap((event): AgentUpdate[] => {
    const labels: Record<string, string> = {
      turn_started: "Started a turn", turn_finished: "Finished a turn",
      task_done: "Task completed", task_running: "Task started", task_failed: "Task failed",
    };
    const label = labels[event.kind];
    if (!label) return [];
    return [{ key: `event-${event.seq}`, label, text: event.task_id ?? "", at: event.at }];
  });
  const notes = entries.flatMap((raw): AgentUpdate[] => {
    const entry = asEntry(raw);
    if (!entry || entry.role !== "agent" || !entry.text.trim()) return [];
    if (entry.kind !== "assistant_text" && entry.kind !== "turn_result") return [];
    // Only provider-authored commentary/results, never tool output or operator prompts.
    return [{ key: `note-${entry.seq}`, label: entry.kind === "turn_result" ? "Latest result" : "Progress update", text: entry.text, at: entry.created ?? null }];
  });
  return [...events, ...notes].sort((a, b) =>
    (Date.parse(b.at ?? "") || 0) - (Date.parse(a.at ?? "") || 0),
  ).slice(0, 3);
}

export function recentWorkingAgents(agents: Agent[]): Agent[] {
  return agents.filter((a) => a.alias !== "master" &&
    (a.running > 0 || a.queued > 0 || a.on.length > 0 || !!a.last_activity || (a.event_cursor ?? 0) > 0),
  ).sort((a, b) => (Date.parse(b.last_activity ?? "") || 0) - (Date.parse(a.last_activity ?? "") || 0) ||
    (b.event_cursor ?? 0) - (a.event_cursor ?? 0))
    .slice(0, 12);
}

/** Worker reports already delivered to Master are useful even when a provider has no thread. */
export function reportedAgentUpdates(entries: unknown[]): Map<string, AgentUpdate[]> {
  const reports = new Map<string, AgentUpdate[]>();
  for (const raw of entries) {
    const entry = asEntry(raw);
    if (!entry || entry.role !== "system") continue;
    const match = entry.text.match(/^\[report\]\s+([A-Z]+-\d+)\s+(done|progress)\s+by\s+(\S+)\s+at\s+(\S+)/);
    if (!match) continue;
    const [, issue, kind, alias, at] = match;
    const summary = entry.text.split(/\n#+\s*Expected\s*\n/i)[1]?.split(/\n#+\s*Evidence/i)[0]?.trim();
    const rows = reports.get(alias) ?? [];
    rows.push({ key: `report-${entry.seq}`, label: kind === "done" ? "Work reported" : "Progress report", text: `${issue}${summary ? ` — ${summary}` : " — Report delivered to Master"}`, at });
    reports.set(alias, rows.sort((a, b) => (Date.parse(b.at ?? "") || 0) - (Date.parse(a.at ?? "") || 0)).slice(0, 3));
  }
  return reports;
}
