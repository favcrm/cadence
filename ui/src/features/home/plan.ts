import type { IssueDetail, PlanTicket } from "../../lib/types";

/**
 * The plan card's view of a plan epic (CAD-328): goal, tickets with size
 * and acceptance, progress, and which decisions the operator may take.
 * Read from the epic's issue detail (`GET /api/issues/<epic>`, whose
 * `plan` block CAD-360 added), through this adapter so a missing field
 * shows as unknown instead of breaking the card. Tested in
 * tests/planCard.test.ts.
 */

export interface PlanView {
  epic: string;
  title: string;
  goal: string | null;
  state: string;
  proposedBy: string | null;
  decidedBy: string | null;
  reason: string | null;
  tickets: PlanTicket[];
  /** 0–100, size-weighted; null when unknown. */
  percent: number | null;
  done: number;
  total: number;
  /** Approve and Reject are offered. */
  canDecide: boolean;
  /** Why they are not, when the plan is still proposed. */
  blockedReason: string | null;
}

/** The `## Goal` section of a plan epic's body (`plan propose` writes it). */
export function planGoal(body: string | null | undefined): string | null {
  if (!body) return null;
  const m = /^## Goal\s*\n+([\s\S]*?)(?=\n## |\s*$)/m.exec(body);
  const goal = m?.[1].trim();
  return goal ? goal : null;
}

/** `block` is why this board cannot write (read-only, not signed in), or null. */
export function planView(detail: IssueDetail, block: string | null): PlanView | null {
  const plan = detail.plan;
  if (!plan) return null;
  const tickets = Array.isArray(plan.tickets) ? plan.tickets : [];
  const ratio = plan.progress?.ratio;
  const proposed = plan.state === "proposed";
  return {
    epic: detail.id,
    title: detail.title,
    goal: planGoal(detail.body),
    state: plan.state ?? "unknown",
    proposedBy: plan.proposed_by ?? null,
    decidedBy: plan.decided_by ?? null,
    reason: plan.reason ?? null,
    tickets,
    percent: typeof ratio === "number" ? Math.round(ratio * 100) : null,
    done: tickets.filter((t) => t.status === "done").length,
    total: tickets.filter((t) => t.status !== "dropped").length,
    canDecide: proposed && !block,
    blockedReason: proposed && block ? `${block} Or decide with cadence plan approve.` : null,
  };
}

/** A decision the card is about to send, validated before any request. */
export function rejectReasonError(reason: string): string | null {
  return reason.trim() ? null : "Say why — the reason goes on the plan for whoever proposed it.";
}
