import type { Agent, IssueCard } from "./types";

/**
 * The issue side of the agent ↔ project join, built once per issue list.
 * `ownedBy` mirrors the CLI `cadence status` ISSUES column
 * (`status_view` in src/main.rs): an issue belongs to its `owner` alias
 * while its derived status is `doing` or `review`. One known gap: card
 * status here includes the job overlay (`views_with_jobs`), the CLI reads
 * notes-only `views`, so a job-overlaid status can differ between them.
 */
export interface IssueIndex {
  projectOf: Map<string, string>;
  ownedBy: Map<string, string[]>;
}

const OWNED_STATUSES = new Set(["doing", "review"]);

/** The only ownership join used by project-scoped UI views. */
export function issueIndex(issues: IssueCard[]): IssueIndex {
  const projectOf = new Map<string, string>();
  const ownedBy = new Map<string, string[]>();
  for (const issue of issues) {
    projectOf.set(issue.id, issue.project);
    if (issue.owner && OWNED_STATUSES.has(issue.status)) {
      ownedBy.set(issue.owner, [...(ownedBy.get(issue.owner) ?? []), issue.id]);
    }
  }
  return { projectOf, ownedBy };
}

/**
 * Every issue an agent is bound to. Two sources, either one is enough:
 * dispatch — the agent's tasks joined through `jobs.issue_id`
 * (`on`/`tasks` from `GET /api/agents`), and ownership — the issues it
 * owns in `doing`/`review` (`owner`/`status` from `GET /api/issues`),
 * the same rule as the CLI ISSUES column.
 */
export function agentIssueIds(agent: Agent, index: IssueIndex): string[] {
  const ids = new Set<string>(agent.on);
  for (const task of agent.tasks ?? []) {
    if (task.issue) ids.add(task.issue);
  }
  for (const id of index.ownedBy.get(agent.alias) ?? []) ids.add(id);
  return [...ids];
}

/** Projects of the bound issues. An empty result is explicitly unassigned. */
export function agentProjectKeys(agent: Agent, index: IssueIndex): Set<string> {
  return new Set(
    agentIssueIds(agent, index)
      .map((id) => index.projectOf.get(id))
      .filter((key): key is string => Boolean(key)),
  );
}

export function agentMatchesProject(agent: Agent, project: string, index: IssueIndex): boolean {
  return project === "all" || agentProjectKeys(agent, index).has(project);
}

export function agentIsUnassigned(agent: Agent, index: IssueIndex): boolean {
  return agentProjectKeys(agent, index).size === 0;
}
