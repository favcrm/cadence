import {
  boardHeadline,
  boardScope,
  boardVisible,
  countLabel,
  issueCounts,
  statusBreakdown,
} from "./counts";
import { NO_FILTERS } from "./filters";
import type { IssueCard } from "./types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function card(id: string, project: string, status: string, extra: Partial<IssueCard> = {}): IssueCard {
  return {
    id,
    project,
    title: `${id} title`,
    status,
    status_source: "file",
    priority: "P2",
    blocked_by: [],
    relates: [],
    refs: [],
    container: false,
    ready: false,
    blocked: false,
    created: "2026-09-01T00:00:00Z",
    rev: id,
    counts: { comments: 0, artifacts: 0, refs: 0 },
    checks: { done: 0, total: 0 },
    ...extra,
  } as IssueCard;
}

// The audit's disagreement: epics, done and dropped counted by some views
// and not others. One fixture with all of them in two projects.
const issues: IssueCard[] = [
  card("CAD-1", "cadence", "doing", { owner: "opus-n" }),
  card("CAD-2", "cadence", "review"),
  card("CAD-3", "cadence", "ready", { tags: ["ui"] }),
  card("CAD-4", "cadence", "done"),
  card("CAD-5", "cadence", "done"),
  card("CAD-6", "cadence", "dropped"),
  // An epic rolled up to doing — the "overview doing 10 vs board 5" case.
  card("CAD-7", "cadence", "doing", { container: true }),
  card("KID-1", "kidult", "backlog"),
  card("KID-2", "kidult", "doing"),
  card("KID-3", "kidult", "done"),
];

const all = issueCounts(issues);
const cadence = issueCounts(issues, "cadence");
const kidult = issueCounts(issues, "kidult");

equal(cadence, { open: 3, byStatus: { doing: 1, review: 1, ready: 1 }, done: 2, dropped: 1, containers: 1 }, "cadence counts");
equal(countLabel(cadence), "3 open · 2 done hidden · 1 dropped · 1 epic", "labelled exclusions");
equal(countLabel(issueCounts([], "cadence")), "0 open", "empty project label");
equal(statusBreakdown(cadence), "doing:1  ready:1  review:1", "overview breakdown");

// Every issue is either counted or named under an exclusion.
for (const c of [all, cadence, kidult]) {
  const total = c.open + c.done + c.dropped + c.containers;
  const expected = c === all ? issues.length : issues.filter((i) => i.project === (c === cadence ? "cadence" : "kidult")).length;
  equal(total, expected, "nothing silently dropped");
}

// Sidebar: "All projects" is the sum of its projects.
equal(all.open, cadence.open + kidult.open, "sidebar all = sum of projects");

// Sidebar, board header and overview agree for every scope.
for (const project of ["all", "cadence", "kidult"]) {
  const counts = issueCounts(issues, project);
  // Sidebar number.
  const sidebar = counts.open;
  // Board: the un-narrowed default view renders exactly the open cards,
  // and its header leads with the same number.
  const visible = boardVisible(boardScope(issues, project, ""), NO_FILTERS, "");
  equal(visible.length, sidebar, `${project}: board renders the sidebar count`);
  const headline = boardHeadline(counts, visible, NO_FILTERS, "");
  equal(headline.startsWith(`${sidebar} open`), true, `${project}: board header "${headline}"`);
  // Board columns agree with the overview breakdown status by status.
  for (const [status, n] of Object.entries(counts.byStatus)) {
    equal(visible.filter((t) => t.status === status).length, n, `${project}: ${status} column = overview`);
  }
  // Overview: the per-status summary sums to the same number.
  const overview = Object.values(counts.byStatus).reduce((a, b) => a + b, 0);
  equal(overview, sidebar, `${project}: overview summary = sidebar`);
}

// Board header while narrowed says how much of the open count is shown.
{
  const filters = { ...NO_FILTERS, tags: ["ui"] };
  const visible = boardVisible(boardScope(issues, "cadence", ""), filters, "");
  equal(boardHeadline(cadence, visible, filters, ""), "1 of 3 open · 2 done hidden · 1 dropped · 1 epic", "narrowed header");
}
// Showing done flips the label, not the open count.
{
  const filters = { ...NO_FILTERS, showDone: true };
  const visible = boardVisible(boardScope(issues, "cadence", ""), filters, "");
  equal(visible.length, cadence.open + cadence.done, "done shown adds done cards");
  equal(boardHeadline(cadence, visible, filters, ""), "3 open · 2 done shown · 1 dropped · 1 epic", "done shown header");
}
// A search looks through done work but the open count stays the base.
{
  const visible = boardVisible(boardScope(issues, "cadence", "cad-4"), NO_FILTERS, "cad-4");
  equal(visible.map((t) => t.id), ["CAD-4"], "search finds done");
  equal(boardHeadline(cadence, visible, NO_FILTERS, "cad-4"), "0 of 3 open · 2 done hidden · 1 dropped · 1 epic", "search header");
}

console.log("count checks passed");
