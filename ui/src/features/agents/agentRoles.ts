/** CAD-1752: the role catalogue's client side - the load hook, the name
 *  suggestion and the plain-words helpers the Recruit screens share. */

import { useCallback, useEffect, useState } from "react";
import { api } from "../../lib/api";
import type { Agent, AgentRole, AgentRolesPayload, AppRow } from "../../lib/types";

export interface RolesState {
  data: AgentRolesPayload | null;
  error: string | null;
  retry: () => void;
}

/** Loads `GET /api/agent-roles` once per mount; any signed-in viewer reads it. */
export function useAgentRoles(): RolesState {
  const [data, setData] = useState<AgentRolesPayload | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let live = true;
    setError(null);
    api
      .agentRoles()
      .then((payload) => live && setData(payload))
      .catch((e) => live && setError(String(e?.message ?? e)));
    return () => {
      live = false;
    };
  }, [attempt]);
  const retry = useCallback(() => setAttempt((n) => n + 1), []);
  return { data, error, retry };
}

/** First free `<role>-<n>` against the current roster. */
export function nextAlias(roleId: string, taken: Iterable<string>): string {
  const used = new Set(taken);
  let n = 1;
  while (used.has(`${roleId}-${n}`)) n += 1;
  return `${roleId}-${n}`;
}

const ALIAS_GRAMMAR = /^[A-Za-z0-9._-]{1,80}$/;

/** Why a hire name cannot be used, or null. */
export function aliasProblem(alias: string, taken: Iterable<string>): string | null {
  const name = alias.trim();
  if (!name) return "A name is required.";
  if (!ALIAS_GRAMMAR.test(name)) return "Use letters, digits, dots, dashes and underscores.";
  for (const other of taken) if (other === name) return "An agent with this name already exists.";
  return null;
}

/** Known app ids → the roles that work in them. */
const APP_ROLE_HINTS: { match: RegExp; label: string; roles: string[] }[] = [
  { match: /crm/i, label: "CRM", roles: ["quotation", "sales"] },
  { match: /invoice/i, label: "Invoices", roles: ["accountant"] },
];

/** Roles suggested by the installed apps, with the app labels that matched. */
export function suggestedRoles(
  apps: Pick<AppRow, "name" | "title">[],
  roles: AgentRole[],
): { roles: AgentRole[]; labels: string[] } {
  const ids: string[] = [];
  const labels: string[] = [];
  for (const hint of APP_ROLE_HINTS) {
    const hit = apps.some((app) => hint.match.test(`${app.name ?? ""} ${app.title ?? ""}`));
    if (!hit) continue;
    labels.push(hint.label);
    for (const id of hint.roles) if (!ids.includes(id)) ids.push(id);
  }
  return {
    roles: ids.flatMap((id) => roles.filter((role) => role.id === id)),
    labels,
  };
}

export function categoryLabel(payload: AgentRolesPayload | null, id: string): string {
  return payload?.categories.find((c) => c.id === id)?.label ?? id;
}

/** The catalogue role an agent was hired from, if it carries one. */
export function roleOf(
  agent: Pick<Agent, "params" | "catalogue">,
  payload: AgentRolesPayload | null,
): AgentRole | null {
  const id = agent.catalogue ?? agent.params?.catalogue;
  return typeof id === "string" ? (payload?.roles.find((r) => r.id === id) ?? null) : null;
}

export function onTeamCount(role: AgentRole, agents: Pick<Agent, "params" | "catalogue">[]): number {
  return agents.filter((a) => (a.catalogue ?? a.params?.catalogue) === role.id).length;
}

export function matchesRole(role: AgentRole, category: string, query: string): boolean {
  const q = query.trim().toLowerCase();
  return (
    (category === "all" || role.category === category) &&
    (!q || `${role.title} ${role.job}`.toLowerCase().includes(q))
  );
}

export function runsOnText(hosted: boolean): string {
  return hosted ? "your AgenticOS workspace" : "this computer";
}

/** A model source such as `role_default` in words. */
export function modelSourceText(role: AgentRole): string | null {
  if ("error" in role.model) return null;
  const source = role.model.source.replace(/_/g, " ");
  return `${source} (${role.team_role})`;
}

/** The edit-job reply in plain words. */
export function appliesText(applies: string): string {
  if (applies === "next_turn") return "Applies from its next turn";
  if (applies === "next_launch") return "Applies the next time it launches";
  return "Saved, but this agent does not read it yet";
}

/** "18 min", "45 s": a turn length in plain words; null when unknown. */
export function durationText(secs: number | null | undefined): string {
  if (secs === null || secs === undefined) return "—";
  if (secs < 90) return `${Math.round(secs)} s`;
  if (secs < 5400) return `${Math.round(secs / 60)} min`;
  return `${(secs / 3600).toFixed(1)} h`;
}

/** "3 Oct", from epoch seconds; "—" when unknown. */
export function hiredText(epochSecs: number | null | undefined): string {
  if (epochSecs === null || epochSecs === undefined) return "—";
  return new Date(epochSecs * 1000).toLocaleDateString("en-GB", { day: "numeric", month: "short" });
}
