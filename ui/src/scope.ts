import type { Agent, IssueCard } from "./types";

/** The only ownership join used by project-scoped UI views. */
export function issueProjectMap(issues: IssueCard[]): Map<string, string> {
  return new Map(issues.map((issue) => [issue.id, issue.project]));
}

export function agentIssueIds(agent: Agent): string[] {
  const ids = new Set<string>(agent.on);
  for (const task of agent.tasks ?? []) {
    if (task.issue) ids.add(task.issue);
  }
  return [...ids];
}

/** Exact issue/job bindings only. An empty result is explicitly unassigned. */
export function agentProjectKeys(
  agent: Agent,
  issueProjects: Map<string, string>,
): Set<string> {
  return new Set(
    agentIssueIds(agent)
      .map((id) => issueProjects.get(id))
      .filter((key): key is string => Boolean(key)),
  );
}

export function agentMatchesProject(
  agent: Agent,
  project: string,
  issueProjects: Map<string, string>,
): boolean {
  return project === "all" || agentProjectKeys(agent, issueProjects).has(project);
}

export function agentIsUnassigned(
  agent: Agent,
  issueProjects: Map<string, string>,
): boolean {
  return agentProjectKeys(agent, issueProjects).size === 0;
}
