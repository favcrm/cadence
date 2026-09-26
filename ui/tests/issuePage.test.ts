import type { IssueDetail, IssueHistoryEntry } from "../src/lib/types";
import {
  acceptanceItems,
  approveReason,
  askAgentReason,
  briefPreview,
  deliveryStages,
  issuePath,
  kickoffBlock,
  kickoffRequest,
  navMatches,
  NO_ACCEPTANCE,
  parseIssueTab,
  peekSummary,
  refreshIssueIds,
  shownLinks,
  timelineRows,
} from "../src/features/issues/model";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function detail(over: Partial<IssueDetail> = {}): IssueDetail {
  return {
    id: "CAD-1",
    project: "cadence",
    title: "Issue detail page",
    status: "backlog",
    status_source: "file",
    priority: "P2",
    blocked_by: [],
    relates: [],
    refs: [],
    container: false,
    ready: false,
    blocked: false,
    created: "2026-09-26T00:00:00Z",
    rev: "r",
    counts: { comments: 0, artifacts: 0, refs: 0 },
    checks: { done: 0, total: 0 },
    frontmatter: {},
    body: "",
    path: "issue.md",
    links: { children: [], blocked_by: [], blocks: [], relates: [], duplicates: [] },
    comments: [],
    notes_chain: [],
    artifacts: [],
    activity: [],
    ...over,
  };
}

const body = [
  "Goal line.",
  "",
  "## Acceptance",
  "- [ ] Header shows Kick off",
  "- [x] <script>alert(1)</script>",
  "```",
  "- [ ] hidden in a fence",
  "```",
  "- [ ] ",
  "## Later",
  "- [ ] not in acceptance",
].join("\n");

equal(
  acceptanceItems(body).map((i) => [i.checked, i.text]),
  [[false, "Header shows Kick off"], [true, "<script>alert(1)</script>"]],
  "acceptance items skip fences, stubs, and later sections; text stays text",
);
equal(acceptanceItems("## Acceptance\n- [ ] a\n## Acceptance\n- [ ] b"), [], "duplicate heading yields nothing");
equal(acceptanceItems("```\n## Acceptance\n- [ ] inside\n```"), [], "fenced heading is not a section");

equal(kickoffBlock([], null), NO_ACCEPTANCE, "no acceptance blocks kick off");
equal(kickoffBlock(acceptanceItems(body), null), null, "checks allow kick off");
equal(kickoffBlock(acceptanceItems(body), "Writes are off."), "Writes are off.", "a write block still wins");

equal(approveReason([]), "No pull request yet", "approve without a PR");
equal(approveReason([{ kind: "pr" }]).includes("cadence audit approve"), true, "approve names the missing route");
equal(askAgentReason(0), "No lane yet", "ask needs a lane");
equal(askAgentReason(1), null, "a bound agent can be asked");

equal(issuePath("cadence", "CAD 1", "pr"), "/projects/cadence/issues/CAD%201?tab=pr", "issue href");
equal(parseIssueTab("conversation"), "conversation", "known tab");
equal(parseIssueTab("drawer"), "overview", "unknown tab");
equal(navMatches("issue", "projects"), true, "issues stay under Projects");
equal(navMatches("issue", "home"), false, "no top-level issue nav");

equal(refreshIssueIds(null, "CAD-607"), ["CAD-607"], "the issue page refreshes when the peek is closed");
equal(refreshIssueIds("CAD-1", null), ["CAD-1"], "a peek still refreshes");
equal(refreshIssueIds("CAD-1", "CAD-607"), ["CAD-1", "CAD-607"], "peek and page both refresh");
equal(refreshIssueIds("CAD-607", "CAD-607"), ["CAD-607"], "one id is asked once");

const linkRef = (id: string) => ({ id, title: id, missing: false });
const shown = shownLinks({
  parent: linkRef("CAD-P"),
  children: [linkRef("CAD-C")],
  blocked_by: [linkRef("CAD-B")],
  blocks: [linkRef("CAD-K")],
  relates: [linkRef("CAD-R")],
  duplicate_of: linkRef("CAD-D"),
  duplicates: [linkRef("CAD-U")],
});
equal(
  shown.map((row) => [row.label, row.unlinkKind]),
  [
    ["Parent", "parent"],
    ["Child", null],
    ["Blocked by", "blocked_by"],
    ["Blocks", null],
    ["Related", "relates"],
    ["Dup of", "duplicate_of"],
    ["Dup", null],
  ],
  "children and duplicates render; only writable kinds unlink",
);

equal(
  kickoffRequest("CAD/1", { group: "west-pm", provider: "cursor", model: "grok-4.7-high", effort: "high", note: "  " }),
  { path: "/api/issues/CAD%2F1/kickoff", body: { group: "west-pm", provider: "cursor", model: "grok-4.7-high", effort: "high" } },
  "kickoff request drops a blank note",
);
equal(briefPreview("CAD-1", "Title", "Body", "").endsWith("No extra note."), true, "brief preview");
equal(peekSummary("# Title\n\nRead the goal."), "Read the goal.", "peek skips the heading");
equal(peekSummary("<img src=x onerror=alert(1)>").includes("<img"), true, "peek summary is raw text");

equal(deliveryStages(detail()), null, "a backlog issue has no delivery timeline");
const doing = detail({
  status: "doing",
  agents: [{ alias: "lane-1", task: "t", task_state: "doing", state: "busy" }],
  refs: [{ kind: "branch", label: "cadence/cad-604-issue-detail-page-with-a-very-long-name" }, { kind: "pr", url: "https://github.com/favcrm/cadence/pull/900", label: "PR #900" }],
  commits: [{ repo: "cadence", sha: "aaa1111bbbb", at: "2026-09-26T09:40:00Z", author: "a", subject: "page" }],
});
const stages = deliveryStages(doing)!;
equal(stages.map((s) => s.title), ["Claimed", "Worktree", "Commits", "PR", "CI", "Reviews", "Queue", "Merged", "Rollout"], "delivery steps");
equal(stages[2].detail.includes("aaa1111"), true, "latest commit");
equal(stages[3].detail, "PR #900", "pr step");
equal(stages[4].tone, "wait", "ci is not on the board");

const history: IssueHistoryEntry[] = [{ sha: "abc", at: "2026-09-26T09:00:00Z", by: "op", kind: "set", summary: "status → doing" }];
const rows = timelineRows(
  detail({
    activity: [{ at: "2026-09-26T08:00:00Z", kind: "comment", author: "op", body: "hello <b>there</b>" }],
  }),
  history,
);
equal(rows.map((r) => r.title), ["op", "set"], "comments then tracker events, no fake stages");
equal(rows[0].markdown, true, "comment bodies are markdown");
equal(rows[0].detail, "hello <b>there</b>", "comment text is not rewritten");

console.log("issue page checks passed");
