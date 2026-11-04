/** The agent page's sections; the active one rides the URL as `?tab=`. */
export const AGENT_TABS = [
  { id: "instructions", label: "Instructions" },
  { id: "skills", label: "Skills" },
  { id: "tools", label: "Tools" },
  { id: "history", label: "History" },
] as const;

/** Beyond the strip: `overview` is the chat, `settings` the gear drawer. */
export type AgentTab = (typeof AGENT_TABS)[number]["id"] | "overview" | "settings";

/** Where a link to a section that has moved lands. */
const MOVED_TABS = new Map<string, AgentTab>([
  ["settings", "settings"],
  ["job", "instructions"],
  ["knowledge", "settings"],
  ["access", "settings"],
  ["work", "overview"],
]);

export function parseAgentTab(value: string | null): AgentTab {
  const hit = AGENT_TABS.find((tab) => tab.id === value);
  if (hit) return hit.id;
  return (value !== null && MOVED_TABS.get(value)) || "overview";
}

/** The skills Cadence bundles; every agent on the host loads all of them today. */
export const BUNDLED_SKILLS = [
  { name: "cadence", line: "Tickets, threads and reports" },
  { name: "business-documents", line: "Document intake, templates and pricing checks" },
  { name: "company-outputs", line: "Quotes, invoices, proposals and reports" },
  { name: "website-builder", line: "Build and publish a static website" },
] as const;

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
