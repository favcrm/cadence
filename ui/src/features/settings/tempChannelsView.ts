import { fmtTime } from "../../lib/fmt";
import type {
  TempChannelAuditItem,
  TempChannelSummary,
  TempChannelTriageStatus,
} from "../../lib/types";

const TEMP_CHANNEL_ID_RE = /^tch_[0-9a-f]{32}$/;

export type ConnectionsTab = "services" | "temp-channels";

/** Read initial tab selection from URL query parameters (?tab=temp-channels or ?channel=tch_...). */
export function connectionsTabParam(search: string): ConnectionsTab {
  const params = new URLSearchParams(search);
  if (params.get("tab") === "temp-channels") return "temp-channels";
  if (tempChannelFocusParam(search) !== null) return "temp-channels";
  return "services";
}

/** Optional deep-link to open a specific temp channel drawer (`?channel=tch_<32 hex>`). */
export function tempChannelFocusParam(search: string): string | null {
  const raw = new URLSearchParams(search).get("channel")?.trim() ?? "";
  return TEMP_CHANNEL_ID_RE.test(raw) ? raw : null;
}

export function shortTempChannelId(id: string): string {
  if (id.length <= 18) return id;
  return `${id.slice(0, 12)}…${id.slice(-4)}`;
}

export function tempChannelStatusView(ch: {
  status: TempChannelSummary["status"];
  pending_count?: number;
  needs_human_count?: number;
}): { label: string; tone: "ok" | "warn" | "fail" | "muted" } {
  if (ch.status === "open") {
    if ((ch.needs_human_count ?? 0) > 0) return { label: "Needs human", tone: "warn" };
    if ((ch.pending_count ?? 0) > 0) return { label: "Open · Pending triage", tone: "ok" };
    return { label: "Open", tone: "ok" };
  }
  if (ch.status === "closed") return { label: "Closed", tone: "fail" };
  return { label: "Expired", tone: "muted" };
}

export function triageStatusView(status: TempChannelTriageStatus): {
  label: string;
  tone: "ok" | "warn" | "fail";
} {
  switch (status) {
    case "handled":
      return { label: "Handled", tone: "ok" };
    case "discarded":
      return { label: "Discarded", tone: "fail" };
    case "needs_human":
      return { label: "Needs human", tone: "warn" };
    default:
      return { label: "Pending triage", tone: "warn" };
  }
}

export function formatChannelExpiry(ch: TempChannelSummary, nowMs = Date.now()): string {
  if (ch.status === "closed") {
    const when = ch.closed_at ?? ch.expires_at;
    return `Closed · ${fmtTime(new Date(when).toISOString())}`;
  }
  if (ch.status === "expired") {
    return `Expired · ${fmtTime(new Date(ch.closed_at ?? ch.expires_at).toISOString())}`;
  }
  const diffMs = ch.expires_at - nowMs;
  if (diffMs <= 0) return "Expiring…";
  const mins = Math.max(1, Math.ceil(diffMs / 60_000));
  if (mins < 60) return `${mins}m left`;
  const hrs = Math.floor(mins / 60);
  const rem = mins % 60;
  return rem === 0 ? `${hrs}h left` : `${hrs}h ${rem}m left`;
}

export function tempChannelCountsSummary(item: TempChannelAuditItem): string {
  return `${item.message_count}/${item.max_messages} msgs`;
}
