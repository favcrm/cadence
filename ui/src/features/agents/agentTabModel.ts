/** CAD-1752: the agent page's tabs; the active one rides the URL as `?tab=`. */
export const AGENT_TABS = [
  { id: "overview", label: "Overview" },
  { id: "work", label: "Work" },
  { id: "history", label: "History" },
  { id: "job", label: "Job" },
  { id: "knowledge", label: "Knowledge" },
  { id: "skills", label: "Skills" },
  { id: "tools", label: "MCP & tools" },
  { id: "settings", label: "Settings" },
] as const;

export type AgentTab = (typeof AGENT_TABS)[number]["id"];

export function parseAgentTab(value: string | null): AgentTab {
  const hit = AGENT_TABS.find((tab) => tab.id === value);
  return hit ? hit.id : "overview";
}

/** The four skills Cadence bundles; every agent on the host loads all of them today. */
export const BUNDLED_SKILLS = ["cadence", "business-documents", "company-outputs", "website-builder"] as const;

export type HistoryFilter = "all" | "turns" | "messages" | "reports" | "memory";

export const HISTORY_FILTERS: { id: HistoryFilter; label: string }[] = [
  { id: "all", label: "All" },
  { id: "turns", label: "Turns" },
  { id: "messages", label: "Messages" },
  { id: "reports", label: "Reports" },
  { id: "memory", label: "Memory" },
];

/** Which filter chip an event kind belongs to. */
export function historyKind(kind: string): Exclude<HistoryFilter, "all"> {
  if (/memory|lesson/.test(kind)) return "memory";
  if (/report/.test(kind)) return "reports";
  if (/message|delivery|inbox|thread|nudge|bootstrap/.test(kind)) return "messages";
  return "turns";
}
