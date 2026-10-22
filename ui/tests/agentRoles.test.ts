import { aliasProblem, appliesText, matchesRole, nextAlias, suggestedRoles } from "../src/features/agents/agentRoles";
import { historyKind, parseAgentTab } from "../src/features/agents/agentTabModel";
import { locationHref, readLocation } from "../src/lib/router";
import type { AgentRole } from "../src/lib/types";

function check(condition: boolean, message: string): void {
  if (!condition) throw new Error(`agentRoles: ${message}`);
}

const role = (id: string, category: string, title: string, job: string): AgentRole => ({
  id,
  category,
  title,
  job,
  look: "custom",
  team_role: "worker",
  kind: "apps",
  does: [],
  skills: [],
  tools: [],
  first_tasks: [],
  instructions: "",
  model: { id: "m", source: "role_default" },
});
const roles = [
  role("quotation", "business", "Quotation", "Turns a request into a quote."),
  role("sales", "customer", "Sales follow-up", "Keeps leads warm."),
  role("accountant", "business", "Accountant", "Reconciles invoices."),
  role("developer", "tech", "Developer", "Builds features."),
];

// The hire name: first free <role>-<n>, never a taken or malformed name.
check(nextAlias("developer", []) === "developer-1", "first name");
check(nextAlias("developer", ["developer-1", "developer-2"]) === "developer-3", "skips taken names");
check(aliasProblem("developer-1", ["developer-1"]) !== null, "a taken name is refused");
check(aliasProblem("bad name", []) !== null, "a name outside the alias grammar is refused");
check(aliasProblem("developer-1", []) === null, "a free name passes");

// Suggested for you: from installed apps; nothing matching hides the section.
const crm = suggestedRoles([{ name: "crm", title: "CRM" }], roles);
check(crm.roles.map((r) => r.id).join() === "quotation,sales", "CRM suggests quotation and sales");
check(crm.labels.join() === "CRM", "the matching app is named");
check(suggestedRoles([{ name: "invoices", title: "Invoices" }], roles).roles[0]?.id === "accountant", "Invoices suggests accountant");
check(suggestedRoles([{ name: "notes", title: "Notes" }], roles).roles.length === 0, "no match, no suggestions");

// Catalogue filter: category and search over title and job.
check(matchesRole(roles[0], "business", "") && !matchesRole(roles[0], "tech", ""), "category filter");
check(matchesRole(roles[2], "all", "INVOICE") && !matchesRole(roles[3], "all", "invoice"), "search is case-insensitive over job");

// The edit-job reply in plain words.
check(appliesText("next_turn") === "Applies from its next turn", "next turn wording");
check(appliesText("not_delivered").startsWith("Saved, but"), "undelivered wording is honest");

// Tabs and history chips.
check(parseAgentTab("job") === "job" && parseAgentTab("nope") === "overview", "unknown tab falls back");
check(historyKind("report_filed") === "reports" && historyKind("memory_proposed") === "memory", "history kinds");

// The agent page and catalogue ride the URL.
const page = readLocation("/agents/dev-1", "?tab=job");
check(page.route.screen === "agents" && page.route.alias === "dev-1" && page.route.tab === "job", "tab is read from the query");
check(locationHref(page, "") === "/agents/dev-1?tab=job", "tab is written back");
const plain = readLocation("/agents/dev-1", "");
check(locationHref(plain, "") === "/agents/dev-1", "overview keeps a clean URL");
const recruit = readLocation("/agents", "?recruit=1");
check(recruit.route.screen === "agents" && recruit.route.recruit === true, "recruit view is read");
check(locationHref(recruit, "") === "/agents?recruit=1", "recruit view is written back");
