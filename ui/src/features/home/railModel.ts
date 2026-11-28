import type {
  Agent,
  DocumentFollowUp,
  HostedLocalAssignmentTodo,
  Routine,
  RoutineRun,
  StagedRoutineProposal,
} from "../../lib/types";
import type { TemplateSummary } from "../templates/templatesApi";
import type { StagedOutcome, StagedWrite } from "../decisions/stagedWrite";
import type { AgentUpdate } from "./agentUpdateModel";
import type { HomeNeed } from "./needs";

export type TodoCategory = "all" | "approvals" | "reviews" | "blocked";
export type TodoCategoryKey = Exclude<TodoCategory, "all">;
export type FeedCategory = "all" | "action" | "working" | "uploaded" | "completed" | "external";
export type FeedCategoryKey = Exclude<FeedCategory, "all">;
export type RailTone = "warn" | "info" | "ok" | "fail";

export interface TodoPresentation {
  tone: Exclude<RailTone, "ok">;
  kindLabel: string;
  cueLabel: string;
  sourceLabel: string;
}

export const TODO_CATEGORY_ORDER: readonly { key: TodoCategoryKey; label: string }[] = [
  { key: "approvals", label: "Approvals" },
  { key: "reviews", label: "Reviews" },
  { key: "blocked", label: "Blocked" },
];

export const FEED_CATEGORY_ORDER: readonly { key: FeedCategoryKey; label: string }[] = [
  { key: "action", label: "Action" },
  { key: "working", label: "Working" },
  { key: "uploaded", label: "Uploaded" },
  { key: "completed", label: "Completed" },
  { key: "external", label: "External" },
];

const APPROVAL_NEED_KINDS = new Set(["staged_write", "master_permission", "merge_decision", "approval"]);
const REVIEW_NEED_KINDS = new Set(["plan", "idea_plan", "question"]);

/** Map one `HomeNeed` to its unified To do category pill. */
export function todoCategoryForNeed(need: Pick<HomeNeed, "kind">): TodoCategoryKey {
  if (APPROVAL_NEED_KINDS.has(need.kind)) return "approvals";
  if (REVIEW_NEED_KINDS.has(need.kind)) return "reviews";
  return "blocked";
}

/** Sort priority for unified To do cards: Approvals -> Reviews -> Blocked. */
export function todoPriorityRank(category: TodoCategoryKey): number {
  if (category === "approvals") return 0;
  if (category === "reviews") return 1;
  return 2;
}

/** Calm dot tone, short label, and right-footer cue for a `HomeNeed` card. */
export function todoPresentation(need: Pick<HomeNeed, "kind" | "agent" | "project" | "why">): TodoPresentation {
  const category = todoCategoryForNeed(need);
  const sourceLabel =
    need.agent ??
    (need.project && need.project !== "all" ? need.project : null) ??
    need.why ??
    "Workspace";
  if (category === "approvals") {
    return { tone: "warn", kindLabel: "Approval", cueLabel: "Review", sourceLabel };
  }
  if (category === "reviews") {
    return { tone: "info", kindLabel: "Review", cueLabel: "Review", sourceLabel };
  }
  return { tone: "fail", kindLabel: "Blocked", cueLabel: "Inspect", sourceLabel };
}

export interface LocalAssignmentPresentation {
  tone: "fail" | "info";
  statusLabel: string;
  cueLabel: string;
  sourceLabel: string;
  metaLabel: string;
  summaryText: string;
}

/** Calm presentation for a hosted local assignment surfaced in Home -> To do. */
export function localAssignmentPresentation(item: HostedLocalAssignmentTodo): LocalAssignmentPresentation {
  const { assignment } = item;
  const isBlocked = item.category === "blocked";
  const statusLabel =
    assignment.state === "failed"
      ? "Failed · Local"
      : isBlocked
        ? "Blocked · Local"
        : "Review · Local";
  const sourceLabel = item.runner_label ?? assignment.client_agent_id ?? "local daemon";
  const metaParts: string[] = [item.issue_id];
  if (typeof assignment.generation === "number") metaParts.push(`g${assignment.generation}`);
  const model = assignment.spawn?.model ?? assignment.resume?.model;
  if (model) metaParts.push(model);
  const summaryText =
    assignment.text?.trim() ||
    (isBlocked
      ? "Local worker requires retry or cancellation."
      : "Local worker finished and is awaiting owner review.");
  return {
    tone: isBlocked ? "fail" : "info",
    statusLabel,
    cueLabel: isBlocked ? "Inspect" : "Review",
    sourceLabel,
    metaLabel: metaParts.join(" · "),
    summaryText,
  };
}

export type UnifiedTodoItem =
  | { kind: "need"; key: string; category: TodoCategoryKey; need: HomeNeed }
  | { kind: "routine_step_hold"; key: string; category: "approvals"; run: RoutineRun }
  | { kind: "routine_proposal"; key: string; category: "approvals"; proposal: StagedRoutineProposal }
  | { kind: "waiting_routine"; key: string; category: "reviews"; routine: Routine }
  | { kind: "document"; key: string; category: "reviews"; doc: DocumentFollowUp }
  | { kind: "template"; key: string; category: "reviews"; template: TemplateSummary }
  | { kind: "local_assignment"; key: string; category: "blocked" | "reviews"; item: HostedLocalAssignmentTodo };

/** Stably sort all pending To do cards by urgency: Approvals -> Reviews -> Blocked. */
export function sortUnifiedTodos(items: readonly UnifiedTodoItem[]): UnifiedTodoItem[] {
  return items
    .map((item, index) => ({ item, index }))
    .sort((a, b) => todoPriorityRank(a.item.category) - todoPriorityRank(b.item.category) || a.index - b.index)
    .map(({ item }) => item);
}

/** Return the ordered To do categories that have at least one pending card. */
export function presentTodoCategories(
  items: readonly { category: TodoCategoryKey }[],
): { key: TodoCategoryKey; label: string; count: number }[] {
  return TODO_CATEGORY_ORDER.map((entry) => ({
    ...entry,
    count: items.filter((item) => item.category === entry.key).length,
  })).filter((entry) => entry.count > 0);
}

export function feedCategoriesForDecidedWrite(write: Pick<StagedWrite, "connected">): FeedCategoryKey[] {
  return write.connected !== null ? ["completed", "external"] : ["completed"];
}

const UPLOAD_HINT = /\b(upload|uploaded|file|files|doc|document|wiki|template|deliverable|artifact|pdf|png|html|md)\b/i;
const EXTERNAL_HINT = /\b(send|sent|publish|published|external|email|whatsapp|webhook)\b/i;

export function feedCategoriesForUpdate(update: Pick<AgentUpdate, "label" | "text">): FeedCategoryKey[] {
  const hay = `${update.label} ${update.text}`;
  const out = new Set<FeedCategoryKey>();
  out.add(UPLOAD_HINT.test(hay) ? "uploaded" : "completed");
  if (EXTERNAL_HINT.test(hay)) out.add("external");
  return [...out];
}

export function feedCategoriesForAgent(
  agent: Pick<Agent, "running"> | null,
  hasActiveTasks: boolean,
  updates: readonly Pick<AgentUpdate, "label" | "text">[],
): FeedCategoryKey[] {
  const out = new Set<FeedCategoryKey>();
  if ((agent?.running ?? 0) > 0 || hasActiveTasks) out.add("working");
  for (const update of updates) {
    for (const category of feedCategoriesForUpdate(update)) out.add(category);
  }
  if (out.size === 0) out.add("completed");
  return [...out];
}

export function feedCategoriesForInfoNeed(need: Pick<HomeNeed, "kind" | "title">): FeedCategoryKey[] {
  const hay = `${need.kind} ${need.title}`;
  const out = new Set<FeedCategoryKey>();
  out.add(UPLOAD_HINT.test(hay) ? "uploaded" : "completed");
  if (EXTERNAL_HINT.test(hay)) out.add("external");
  return [...out];
}

export function countFeedCategories(
  entries: readonly { categories: readonly FeedCategoryKey[] }[],
): Record<FeedCategory, number> {
  const counts: Record<FeedCategory, number> = {
    all: entries.length,
    action: 0,
    working: 0,
    uploaded: 0,
    completed: 0,
    external: 0,
  };
  for (const entry of entries) {
    for (const category of entry.categories) counts[category] += 1;
  }
  return counts;
}

/** Shared summary copy for waiting routines in both the rail card and drawer. */
export function waitingRoutineReason(
  routine: Pick<Routine, "status" | "approval_drift" | "approved_rev" | "approved_sha256">,
  t: (english: string) => string,
): string {
  if (routine.status === "repoint_required") return t("Choose thread");
  if (routine.approval_drift && (routine.approved_rev !== null || routine.approved_sha256 !== null)) {
    return t("Playbook or script updated — approve to run");
  }
  return t("Approve routine before first run");
}

/** Shared status label for decided staged writes in Feed cards and the Feed drawer. */
export function decidedWriteStatusLabel(
  write: Pick<StagedWrite, "kind" | "result" | "connected" | "batch">,
  outcome: StagedOutcome,
  t: (english: string) => string,
): string {
  if (write.kind === "action_batch" && write.batch) {
    if (write.batch.status === "denied") return t("Denied");
    if (write.batch.status === "completed") {
      return t("Completed · {done}/{total}")
        .replace("{done}", String(write.batch.completedCount))
        .replace("{total}", String(write.batch.totalCount));
    }
    return t("Partial failure · {done}/{total}")
      .replace("{done}", String(write.batch.completedCount))
      .replace("{total}", String(write.batch.totalCount));
  }
  if (outcome === "failed") return t("Did not apply");
  if (outcome === "unconfirmed") return t("Result not confirmed");
  if (outcome === "denied") return t("Denied");
  if (write.kind === "create_issue" && write.result?.ticket) return `${t("Created")} ${write.result.ticket}`;
  if (write.connected !== null) return t(write.connected.effect === "send" ? "Sent" : "Drafted");
  return t("Applied");
}
