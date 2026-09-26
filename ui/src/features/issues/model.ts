import type { CardAgent, IssueDetail, IssueHistoryEntry, Ref, TaskReport } from "../../lib/types";

/** Tabs on `/projects/<project>/issues/<id>`. `overview` is the bare path. */
export type IssueTab = "overview" | "activity" | "conversation" | "pr" | "evidence";

const TABS: IssueTab[] = ["overview", "activity", "conversation", "pr", "evidence"];

export function parseIssueTab(value: string | null): IssueTab {
  return TABS.includes(value as IssueTab) ? (value as IssueTab) : "overview";
}

export function issuePath(project: string, id: string, tab: IssueTab = "overview"): string {
  const base = `/projects/${encodeURIComponent(project)}/issues/${encodeURIComponent(id)}`;
  return tab === "overview" ? base : `${base}?tab=${tab}`;
}

/** Highlight Projects while an issue page is open. Issues stay under Projects. */
export function navMatches(screen: string, item: string): boolean {
  if (screen === item) return true;
  return screen === "issue" && item === "projects";
}

export interface AcceptanceItem {
  text: string;
  checked: boolean;
}

function markdownLine(line: string): string | null {
  let indent = 0;
  while (indent < line.length && line[indent] === " ") indent += 1;
  return indent <= 3 ? line.slice(indent) : null;
}

function heading(line: string): { level: number; title: string } | null {
  const text = markdownLine(line);
  if (!text || !text.startsWith("#")) return null;
  let level = 0;
  while (level < text.length && text[level] === "#") level += 1;
  if (level < 1 || level > 6) return null;
  const rest = text.slice(level);
  if (rest.length > 0 && !/\s/.test(rest[0])) return null;
  let title = rest.trim().replace(/\s+#+\s*$/, "");
  title = title.trim();
  return title ? { level, title } : null;
}

function fenceMarker(line: string): { marker: string; count: number } | null {
  const text = markdownLine(line);
  if (!text) return null;
  const marker = text[0];
  if (marker !== "`" && marker !== "~") return null;
  let count = 0;
  while (count < text.length && text[count] === marker) count += 1;
  return count >= 3 ? { marker, count } : null;
}

function isClosingFence(line: string, marker: string, count: number): boolean {
  const text = markdownLine(line);
  if (!text || !text.startsWith(marker)) return false;
  let n = 0;
  while (n < text.length && text[n] === marker) n += 1;
  if (n < count) return false;
  return /^[\s]*$/.test(text.slice(n));
}

function checkboxItem(line: string): AcceptanceItem | null {
  const text = markdownLine(line);
  if (!text?.startsWith("- [")) return null;
  const mark = text[3];
  if (text[4] !== "]") return null;
  const checked = mark === "x" || mark === "X";
  if (!checked && mark !== " ") return null;
  const suffix = text.slice(5);
  if (suffix.length > 0 && !/\s/.test(suffix[0])) return null;
  const body = suffix.trim();
  return body ? { text: body, checked } : null;
}

/**
 * Checkboxes under the single level-two `Acceptance` heading.
 * A duplicate heading, a fence, or an empty stub (`- [ ]`) is not an item.
 * Mirrors `acceptance_items` in `src/issue/parse.rs`.
 */
export function acceptanceItems(body: string): AcceptanceItem[] {
  const lines = body.split(/\r?\n/);
  const sections: { start: number; end: number }[] = [];
  let current: number | null = null;
  let fence: { marker: string; count: number } | null = null;
  for (let i = 0; i < lines.length; i++) {
    const marker = fenceMarker(lines[i]);
    if (marker) {
      if (fence && fence.marker === marker.marker && isClosingFence(lines[i], fence.marker, fence.count)) fence = null;
      else if (!fence) fence = marker;
      continue;
    }
    if (fence) continue;
    const h = heading(lines[i]);
    if (!h || h.level > 2) continue;
    if (current !== null) {
      sections[current].end = i;
      current = null;
    }
    if (h.level === 2 && h.title.toLowerCase() === "acceptance") {
      sections.push({ start: i + 1, end: lines.length });
      current = sections.length - 1;
    }
  }
  if (sections.length !== 1) return [];
  const slice = lines.slice(sections[0].start, sections[0].end);
  const items: AcceptanceItem[] = [];
  let open: { marker: string; count: number } | null = null;
  for (const line of slice) {
    const marker = fenceMarker(line);
    if (marker) {
      if (open && open.marker === marker.marker && isClosingFence(line, open.marker, open.count)) {
        open = null;
      } else if (!open) open = marker;
      continue;
    }
    if (open) continue;
    const item = checkboxItem(line);
    if (item) items.push(item);
  }
  return items;
}

export const NO_ACCEPTANCE =
  "This issue has no acceptance checklist. Kick off stays blocked until at least one check exists.";

/** Why Kick off cannot run. Null when the dialog may open. */
export function kickoffBlock(items: AcceptanceItem[], writeBlock: string | null): string | null {
  if (items.length === 0) return NO_ACCEPTANCE;
  return writeBlock;
}

/**
 * There is no board route for human-class merge approval. `cadence audit
 * approve` records it. The button stays disabled and says why.
 */
export function approveReason(refs: { kind: string }[]): string {
  if (!refs.some((r) => r.kind === "pr")) return "No pull request yet";
  return "Human-class merge approval has no board route. Record it with cadence audit approve.";
}

export function askAgentReason(agentCount: number): string | null {
  return agentCount > 0 ? null : "No lane yet";
}

export interface PrRef {
  label: string;
  href: string | null;
}

export function prRef(refs: Ref[]): PrRef | null {
  const ref = refs.find((r) => r.kind === "pr");
  if (!ref) return null;
  const fromUrl = ref.url?.match(/\/pull\/(\d+)/);
  const label = ref.label ?? (fromUrl ? `PR #${fromUrl[1]}` : "PR");
  return { label, href: ref.url ?? null };
}

/** Branch ref for the lane card. The card truncates it and keeps the full name on `title`. */
export function laneBranch(refs: Ref[]): string | null {
  const branch = refs.find((r) => r.kind === "branch");
  return branch?.label || branch?.path || null;
}

export function worktreePath(refs: Ref[]): string | null {
  const wt = refs.find((r) => r.kind === "worktree");
  return wt?.path ?? wt?.label ?? null;
}

/** First plain line of the body, for the peek. Titles and this line render as text. */
export function peekSummary(body: string): string {
  for (const raw of body.split(/\r?\n/)) {
    if (heading(raw)) continue;
    const line = raw.replace(/^[-*+]\s+(\[[ xX]\]\s+)?/, "").trim();
    if (!line || line.startsWith("```") || line.startsWith("~~~")) continue;
    return line.length > 180 ? `${line.slice(0, 177)}…` : line;
  }
  return "Open the issue to read the goal and kick off a lane.";
}

export interface KickoffBody {
  group: string;
  provider: string;
  model?: string;
  effort?: string;
  note?: string;
}

/** `POST /api/issues/<id>/kickoff` — the CAD-606 shape. The route may 404 until that PR lands. */
export function kickoffRequest(id: string, body: KickoffBody): { path: string; body: KickoffBody } {
  const note = body.note?.trim();
  return {
    path: `/api/issues/${encodeURIComponent(id)}/kickoff`,
    body: {
      group: body.group,
      provider: body.provider,
      ...(body.model ? { model: body.model } : {}),
      ...(body.effort ? { effort: body.effort } : {}),
      ...(note ? { note } : {}),
    },
  };
}

export function briefPreview(id: string, title: string, body: string, note: string): string {
  const extra = note.trim();
  return `${id} — ${title}\n\n${body.trim()}\n\n— note —\n${extra || "No extra note."}`;
}

export const CI_UNAVAILABLE = "Checks are not on this board yet.";
export const QUEUE_UNAVAILABLE = "Queue position is not on this board yet.";
export const ROLLOUT_UNAVAILABLE = "No production build is on this page yet.";

export interface TimelineRow {
  title: string;
  detail: string;
  at: string | null;
  tone: "done" | "now" | "warn" | "wait";
  /** Comment and report bodies go through the markdown renderer. Everything else is text. */
  markdown: boolean;
}

function reportTone(report: TaskReport): "done" | "warn" {
  const kind = (report.kind ?? "").toLowerCase();
  return kind.includes("revise") || kind.includes("fail") ? "warn" : "done";
}

/** The nine delivery steps, once a lane or any ship signal exists. */
export function deliveryStages(detail: IssueDetail): TimelineRow[] | null {
  const agents = detail.agents ?? [];
  const commits = detail.commits ?? [];
  const pr = prRef(detail.refs);
  const branch = laneBranch(detail.refs);
  const tree = worktreePath(detail.refs);
  const reports = detail.reports ?? [];
  const started =
    agents.length > 0 ||
    commits.length > 0 ||
    pr !== null ||
    branch !== null ||
    tree !== null ||
    detail.status === "doing" ||
    detail.status === "review" ||
    detail.status === "done";
  if (!started) return null;
  const latest = commits[0];
  const merged = detail.status === "done" || commits.some((c) => c.on_default === true);
  const reviewTone = reports.some((r) => reportTone(r) === "warn") ? "warn" : reports.length ? "done" : "wait";
  return [
    {
      title: "Claimed",
      detail: agents.length ? agents.map((a) => `${a.alias} · ${a.state}`).join(", ") : "No agent is bound to this issue yet.",
      at: null,
      tone: agents.length ? "done" : "wait",
      markdown: false,
    },
    {
      title: "Worktree",
      detail: tree ?? branch ?? "No worktree ref yet.",
      at: null,
      tone: tree || branch ? "done" : "wait",
      markdown: false,
    },
    {
      title: "Commits",
      detail: latest ? `${commits.length} commits · latest ${latest.sha.slice(0, 7)}` : "No commits yet.",
      at: latest?.at ?? null,
      tone: commits.length ? "done" : "wait",
      markdown: false,
    },
    {
      title: "PR",
      detail: pr ? pr.label : "Not opened.",
      at: null,
      tone: pr ? "done" : "wait",
      markdown: false,
    },
    {
      title: "CI",
      detail: CI_UNAVAILABLE,
      at: null,
      tone: "wait",
      markdown: false,
    },
    {
      title: "Reviews",
      detail: reports.length
        ? reports.map((r) => `${r.agent ?? r.name}: ${r.kind ?? "report"}`).join(" · ")
        : "No verdicts.",
      at: reports.find((r) => r.at)?.at ?? null,
      tone: reviewTone,
      markdown: false,
    },
    {
      title: "Queue",
      detail: QUEUE_UNAVAILABLE,
      at: null,
      tone: "wait",
      markdown: false,
    },
    {
      title: "Merged",
      detail: merged ? "Marked done, or a commit is on the default branch." : "Not merged.",
      at: null,
      tone: merged ? "done" : "wait",
      markdown: false,
    },
    {
      title: "Rollout",
      detail: ROLLOUT_UNAVAILABLE,
      at: null,
      tone: "wait",
      markdown: false,
    },
  ];
}

function timeKey(at: string | null): number {
  if (!at) return Number.POSITIVE_INFINITY;
  const n = Date.parse(at);
  return Number.isNaN(n) ? Number.POSITIVE_INFINITY : n;
}

/**
 * Delivery steps, then comments, tracker history, and reports, oldest first.
 * An issue with none of those is an empty timeline.
 */
export function timelineRows(detail: IssueDetail, history: IssueHistoryEntry[]): TimelineRow[] {
  const stages = deliveryStages(detail) ?? [];
  const events: TimelineRow[] = [];
  for (const item of detail.activity) {
    if (item.kind === "comment") {
      events.push({
        title: item.author ?? "comment",
        detail: item.body ?? "",
        at: item.at,
        tone: "done",
        markdown: true,
      });
    } else if (item.kind === "note") {
      events.push({
        title: item.note_kind ?? "note",
        detail: item.title ?? item.name ?? "",
        at: item.at,
        tone: "done",
        markdown: false,
      });
    } else {
      events.push({
        title: "Commit",
        detail: `${item.commit ?? ""} ${item.subject ?? ""}`.trim(),
        at: item.at,
        tone: "done",
        markdown: false,
      });
    }
  }
  for (const entry of history) {
    if (entry.kind === "comment") continue;
    events.push({
      title: entry.kind,
      detail: entry.summary,
      at: entry.at,
      tone: "done",
      markdown: false,
    });
  }
  for (const report of detail.reports ?? []) {
    events.push({
      title: `Report ${report.agent ?? report.name}`,
      detail: report.body ?? report.kind ?? "report",
      at: report.at ?? null,
      tone: reportTone(report),
      markdown: Boolean(report.body),
    });
  }
  events.sort((a, b) => timeKey(a.at) - timeKey(b.at));
  return [...stages, ...events];
}

export function laneState(agents: CardAgent[]): string {
  if (agents.length === 0) return "none";
  const hot = agents.find((a) => a.state === "fenced" || a.state === "attention");
  if (hot) return hot.state;
  return agents[0].state || "idle";
}

export interface KickoffChoice {
  id: string;
  label: string;
  effort: string;
  models: { id: string; cost: string }[];
}

/** Local catalog used until `GET /api/issues/<id>/kickoff` answers. */
export const KICKOFF_CHOICES: KickoffChoice[] = [
  { id: "cursor", label: "Cursor", effort: "high", models: [{ id: "grok-4.7-high", cost: "Cursor plan" }] },
  {
    id: "devin",
    label: "Devin",
    effort: "max",
    models: [
      { id: "swe-2-medium", cost: "Free (quota)" },
      { id: "swe-2-high", cost: "Free (quota)" },
      { id: "swe-2-max", cost: "Free (quota)" },
    ],
  },
  {
    id: "openrouter",
    label: "OpenRouter",
    effort: "medium",
    models: [{ id: "openrouter/example-flash", cost: "Paid" }],
  },
];

export const EFFORTS = ["low", "medium", "high", "xhigh", "max"];
