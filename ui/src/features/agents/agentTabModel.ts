/** The agent page's sections; the active one rides the URL as `?tab=`. */
export const AGENT_TABS = [
  { id: "overview", label: "Overview" },
  { id: "instructions", label: "Instructions" },
  { id: "routines", label: "Routines" },
  { id: "skills", label: "Skills" },
  { id: "tools", label: "Tools" },
  { id: "history", label: "History" },
] as const;

/** Beyond the strip: `settings` opens the gear drawer over the active tab. */
export type AgentTab = (typeof AGENT_TABS)[number]["id"] | "settings";

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

/** The skills Cadence bundles; matches `src/skill.rs::SKILL_NAMES`. */
export const BUNDLED_SKILLS = [
  { name: "cadence", line: "Tickets, threads and reports", required: true },
  { name: "business-documents", line: "Document intake, templates and pricing checks", required: false },
  { name: "company-outputs", line: "Quotes, invoices, proposals and reports", required: false },
  { name: "website-builder", line: "Build and publish a static website", required: false },
  { name: "incident-response", line: "Contain and close out incidents across teams", required: false },
  { name: "deliverable-qa", line: "Independent browser and visual QA before release", required: false },
  { name: "cadence-app-dev", line: "Build, audit and package sandboxed Cadence apps", required: false },
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
