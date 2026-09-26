import type { IssueCard } from "../../lib/types";
const closed = new Set(["done", "dropped"]);
export function projectSummary(cards: IssueCard[], project: string) {
  const scoped = cards.filter((card) => project === "all" || card.project === project);
  const tasks = scoped.filter((card) => !card.container && card.work?.type !== "epic");
  const open = tasks.filter((card) => !closed.has(card.status));
  const epics = scoped.filter((card) => card.container || card.work?.type === "epic");
  return {
    open: open.length,
    doing: open.filter((card) => card.status === "doing").length,
    review: open.filter((card) => card.status === "review").length,
    blocked: open.filter((card) => card.blocked).length,
    done: tasks.filter((card) => card.status === "done").length,
    epics: epics.filter((card) => !closed.has(card.status)),
    focus: open.slice().sort((a, b) => {
      const rank = (card: IssueCard) => card.status === "review" ? 0 : card.blocked ? 1 : card.status === "doing" ? 2 : card.status === "ready" ? 3 : 4;
      return rank(a) - rank(b) || a.priority.localeCompare(b.priority) || a.id.localeCompare(b.id, undefined, { numeric: true });
    }).slice(0, 6),
  };
}
