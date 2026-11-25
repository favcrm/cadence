import { agentOn } from "../../lib/agentOn";
import { activityTimeMs } from "../../lib/fmt";
import type { Agent, AgentFeedItem, AgentFeedState } from "../../lib/types";
import { activeTasks } from "../agents/AgentBlocks";
import type { AgentUpdate } from "./agentUpdateModel";
import type { FeedCategory, FeedCategoryKey, RailTone } from "./railModel";

export type FeedBucket = "attention" | "live" | "recent";

export const STALE_HEARTBEAT_SECS = 240;

const FEED_STATE_RANK: Record<AgentFeedState, number> = {
  action_required: 0,
  blocked: 1,
  failed: 2,
  unverifiable: 3,
  working: 4,
  monitoring: 5,
  completed: 6,
};

export function feedBucketForState(state: AgentFeedState): FeedBucket {
  if (state === "action_required" || state === "blocked" || state === "failed") return "attention";
  if (state === "working" || state === "monitoring" || state === "unverifiable") return "live";
  return "recent";
}

export function feedToneForState(state: AgentFeedState): RailTone | "muted" {
  if (state === "action_required" || state === "unverifiable") return "warn";
  if (state === "blocked" || state === "failed") return "fail";
  if (state === "working" || state === "monitoring") return "ok";
  return "muted";
}

export function isLiveFeedState(state: AgentFeedState): boolean {
  return state === "working" || state === "monitoring";
}

export function feedCategoriesForItem(item: AgentFeedItem): FeedCategoryKey[] {
  const out = new Set<FeedCategoryKey>();
  if (feedBucketForState(item.state) === "attention" || item.operator_action) out.add("action");
  out.add(item.category);
  if (feedBucketForState(item.state) === "live") out.add("working");
  return [...out];
}

export function feedItemMatchesFilter(item: AgentFeedItem, filter: FeedCategory): boolean {
  if (filter === "all") return true;
  return feedCategoriesForItem(item).includes(filter);
}

export function formatFeedClock(secs: number): string {
  const clamped = Math.max(0, Math.floor(secs));
  const m = String(Math.floor(clamped / 60)).padStart(2, "0");
  const s = String(clamped % 60).padStart(2, "0");
  return `${m}:${s}`;
}

export function formatFeedAge(secs: number): string {
  const clamped = Math.max(0, Math.floor(secs));
  if (clamped < 60) return `${clamped}s`;
  const m = Math.floor(clamped / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  return `${Math.floor(h / 24)}d`;
}

export function sortFeedItems(items: readonly AgentFeedItem[]): AgentFeedItem[] {
  return [...items].sort((a, b) => {
    const rankDiff = FEED_STATE_RANK[a.state] - FEED_STATE_RANK[b.state];
    if (rankDiff !== 0) return rankDiff;
    return (Date.parse(b.updated_at) || 0) - (Date.parse(a.updated_at) || 0);
  });
}

function summarizeNote(text: string): string {
  return text.replace(/[#*`]/g, "").split("\n").filter(Boolean).slice(0, 2).join(" ").trim().slice(0, 200);
}

/**
 * Fallback projection when `GET /api/agents` predates `feed_items` (or to enrich
 * native agents whose live detail notes arrived via agent/thread reads).
 */
export function buildFallbackFeedItems(input: {
  agents: readonly Agent[];
  reportsByAlias: ReadonlyMap<string, readonly AgentUpdate[]>;
  updatesByAlias?: ReadonlyMap<string, readonly AgentUpdate[]>;
  nowMs?: number;
}): AgentFeedItem[] {
  const nowMs = input.nowMs ?? Date.now();
  const out: AgentFeedItem[] = [];

  for (const agent of input.agents) {
    if (agent.alias === "master") continue;
    const issues = agentOn(agent);
    const active = activeTasks(agent);
    const notes = [
      ...(input.updatesByAlias?.get(agent.alias) ?? []),
      ...(input.reportsByAlias.get(agent.alias) ?? []),
    ].sort((a, b) => (Date.parse(b.at ?? "") || 0) - (Date.parse(a.at ?? "") || 0));
    const latest = notes[0];
    const activityMs = Math.max(
      activityTimeMs(agent.last_activity ?? null),
      activityTimeMs(latest?.at ?? null),
    );
    const hasSignal =
      agent.running > 0 ||
      agent.queued > 0 ||
      agent.fenced === true ||
      issues.length > 0 ||
      active.length > 0 ||
      notes.length > 0 ||
      activityMs > 0;
    if (!hasSignal) continue;

    const silentSecs =
      typeof agent.silent_secs === "number"
        ? agent.silent_secs
        : activityMs > 0
          ? (nowMs - activityMs) / 1000
          : 0;
    const isStale =
      agent.running > 0 && (agent.stalled === true || silentSecs >= STALE_HEARTBEAT_SECS);

    let state: AgentFeedState;
    let stateLabel: string;
    let actionVerb: string;

    if (agent.fenced) {
      state = "blocked";
      stateLabel = "Needs attention";
      actionVerb = "BLOCKED";
    } else if (isStale) {
      state = "unverifiable";
      stateLabel = "Unverifiable";
      actionVerb = "STALE";
    } else if (agent.running > 0) {
      state = "working";
      stateLabel = "Working";
      actionVerb = "WORKING";
    } else if (agent.queued > 0) {
      state = "monitoring";
      stateLabel = "Queued";
      actionVerb = "QUEUED";
    } else {
      state = "completed";
      stateLabel = typeof agent.state_label === "string" && agent.state_label ? agent.state_label : "Idle";
      actionVerb = latest ? "UPDATED" : "IDLE";
    }

    const ticketId = issues[0] ?? latest?.issue ?? null;
    const contextTitle =
      active.find((t) => t.title || t.job_title)?.title ??
      active.find((t) => t.job_title)?.job_title ??
      null;
    const taskTitle =
      contextTitle ??
      (ticketId ? `Assigned work on ${ticketId}` : `${agent.alias} activity`);
    const latestAction =
      (latest?.text ? summarizeNote(latest.text) : null) ??
      (latest?.label ? latest.label : null) ??
      (state === "unverifiable"
        ? "No recent daemon heartbeat past threshold; execution liveness unconfirmed."
        : `${stateLabel} on ${ticketId ?? agent.alias}`);

    const updatedAt = activityMs > 0 ? new Date(activityMs).toISOString().replace(/\.\d{3}Z$/, "Z") : new Date(nowMs).toISOString().replace(/\.\d{3}Z$/, "Z");

    out.push({
      id: `agent:${agent.alias}`,
      alias: agent.alias,
      look: typeof agent.look === "string" ? agent.look : null,
      role: typeof agent.role === "string" && agent.role ? agent.role : agent.provider || "Worker",
      state,
      state_label: stateLabel,
      ticket_id: ticketId,
      task_title: taskTitle,
      action_verb: actionVerb,
      latest_action: latestAction,
      updated_at: updatedAt,
      started_at:
        agent.running > 0 && !isStale && activityMs > 0
          ? new Date(activityMs).toISOString().replace(/\.\d{3}Z$/, "Z")
          : null,
      runner_label: `${agent.provider} · ${agent.endpoint_kind}`,
      category: state === "completed" ? "completed" : "working",
    });
  }

  return sortFeedItems(out);
}

/**
 * Prefer authoritative `serverItems` (`feed_items` from `/api/agents`), and
 * include any live fallback items not already represented by `serverItems`.
 */
export function mergeFeedItems(
  serverItems: readonly AgentFeedItem[] | undefined,
  fallbackItems: readonly AgentFeedItem[],
): AgentFeedItem[] {
  const base = [...(serverItems ?? [])];
  const seenKeys = new Set(
    base.map((item) => `${item.alias.toLowerCase()}::${item.ticket_id ?? ""}`),
  );
  for (const candidate of fallbackItems) {
    const key = `${candidate.alias.toLowerCase()}::${candidate.ticket_id ?? ""}`;
    if (!seenKeys.has(key)) {
      base.push(candidate);
      seenKeys.add(key);
    }
  }
  return sortFeedItems(base).slice(0, 20);
}
