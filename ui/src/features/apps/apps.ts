import type { AppDetail, AppDoctor, AppRow, AppRun, AppWorkflow, WorkflowRow } from "../../lib/types";
import type { HomeNeed } from "../home/needs";
import type { Viewer } from "../projects/work";

/**
 * The Apps screen's view of an installed app (CAD-557) — the adapters
 * between the `/api/apps` rows and what the cards and detail need, kept
 * pure so the rules are unit-tested in plain node (tests/apps.test.ts).
 *
 * Helpers read a `GateRow` — the fields both payloads share — rather
 * than `AppRow` itself: a detail's `workflows` are checked summaries,
 * not the bare names the list row carries.
 */
export type GateRow = Pick<AppRow, "error" | "approval" | "connections" | "source" | "project" | "name">;

/** `/apps/<project>/<name>` — one app's detail route. */
export function appHref(project: string, name: string): string {
  return `/apps/${encodeURIComponent(project)}/${encodeURIComponent(name)}`;
}

/**
 * The `WorkflowRow` the shared run form reads (CAD-563), built from an
 * app detail's checked workflow summary: the whole-app approval is the
 * gate the `<app>/<wf>` preview and propose re-check, and a workflow
 * that fails its checks carries the errors the Workflows screen's app
 * rows show.
 */
export function appWorkflowRow(project: string, app: AppDetail, wf: AppWorkflow): WorkflowRow {
  return {
    project,
    name: wf.name,
    app: app.name ?? undefined,
    title: wf.title ?? null,
    tickets: wf.tickets ?? null,
    inputs: wf.inputs ?? null,
    approved: app.approved ?? null,
    errors: wf.errors,
    error:
      wf.ok === false
        ? (wf.errors ?? []).join("; ") || "this workflow does not pass its checks"
        : undefined,
  };
}

/**
 * The issue a Needs-you row is about, when one is named (CAD-563): the
 * action's own issue for plan/question/merge rows, else the subject
 * when it is an issue (`review_no_pr` and friends). A `report` subject
 * (`D-2/q.md`) is not an issue id, so it is not matched.
 */
export function needIssue(need: HomeNeed): string | null {
  switch (need.action.type) {
    case "plan":
      return need.action.epic;
    case "answer":
      return need.action.issue;
    case "merge":
      return need.action.issue;
    default:
      return need.subject?.kind === "issue" ? need.subject.id : null;
  }
}

/** A run's plain-language state and chip (CAD-563). */
export interface RunState {
  text: string;
  cls: string;
  /** The row links to Needs you — the run waits on the operator. */
  needsYou: boolean;
}

/**
 * A run's plain-language state, in the order an operator meets it: a
 * decided run first (done — the epic rolled up, or every ticket done),
 * then a plan still waiting for the operator's decision, a rejected
 * one, then anything of the run the Needs-you rail holds, else the
 * approved run in progress.
 */
export function runState(run: AppRun, needs: HomeNeed[]): RunState {
  const tickets = new Set(run.plan.tickets.map((t) => t.id));
  const needsYou = needs.some((n) => {
    const issue = needIssue(n);
    return issue !== null && (issue === run.epic || tickets.has(issue));
  });
  const p = run.plan.progress;
  const allDone = p.total_weight > 0 && p.done_weight >= p.total_weight;
  if (run.status === "done" || run.status === "dropped" || (run.plan.state === "approved" && allDone)) {
    return { text: "done", cls: "bg-ok/15 text-ok", needsYou: false };
  }
  if (run.plan.state === "proposed") {
    // The plan decision is itself a Needs-you row — link there even
    // before the overview has loaded.
    return { text: "waiting approval", cls: "bg-warn/10 text-warn", needsYou: true };
  }
  if (run.plan.state === "rejected") {
    return { text: "rejected", cls: "bg-fail/10 text-fail", needsYou };
  }
  if (needsYou) return { text: "needs you", cls: "bg-warn/10 text-warn", needsYou: true };
  return { text: "running", cls: "bg-accent/15 text-accent", needsYou: false };
}

/** `/outbox?item=<effect_id>` — one published item's detail (CAD-546). */
export function outboxHref(effectId: string): string {
  return `/outbox?item=${encodeURIComponent(effectId)}`;
}

/** The approval chip for an app row — a static class per state. */
export function appApprovalChip(row: GateRow): { text: string; cls: string } {
  if (row.error) return { text: "broken", cls: "bg-fail/10 text-fail" };
  switch (row.approval) {
    case "approved":
      return { text: "approved", cls: "bg-ok/15 text-ok" };
    case "changed":
      return { text: "changed since approval", cls: "bg-warn/10 text-warn" };
    case "unknown":
      return { text: "approval unknown", cls: "bg-ink-800 text-ink-400" };
    default:
      return { text: "unapproved", cls: "bg-warn/10 text-warn" };
  }
}

/** Is this app's approval pending — the state Approve answers? */
export function approvalPending(row: GateRow): boolean {
  return !row.error && row.approval !== "approved";
}

/**
 * Why Approve is not offered, or null when it is. The button is the
 * operator's own: hidden for a broken row or an approved app, blocked
 * for a read-only board or a board that cannot prove the operator.
 */
export function approveBlock(row: GateRow, viewer: Viewer): string | null {
  if (row.error || row.approval === "approved") return null;
  if (viewer.readOnly) return "The board is read-only — sign in as the operator to approve an app.";
  if (!viewer.operator) {
    return "Approving an app is the operator's — this board cannot prove the operator to the daemon. `cadence app approve` does it from the shell.";
  }
  return null;
}

/** Declared slots with no effective binding — the ones doctor flags. */
export function unboundSlots(row: GateRow): string[] {
  return (row.connections ?? []).filter((c) => c.bound == null).map((c) => c.slot);
}

/** Where the app came from, in one line — a git install pins its SHA. */
export function sourceLabel(row: GateRow): string | null {
  const src = row.source;
  if (!src) return null;
  if (src.kind === "git") {
    const sha = src.sha ? ` @ ${src.sha.slice(0, 12)}` : "";
    return `git ${src.url ?? ""}${sha}`;
  }
  if (src.kind === "path") return `path ${src.path ?? ""}`;
  return src.kind ?? null;
}

/** The doctor row's findings, flattened to labeled lines for the detail. */
export function doctorFindings(doctor: AppDoctor | null | undefined): { cls: string; text: string }[] {
  if (!doctor) return [];
  const out: { cls: string; text: string }[] = [];
  for (const s of doctor.slots_ok ?? []) {
    const verified = s.verified ? ` — verification ${s.verified}` : "";
    out.push({ cls: "text-ok", text: `${s.slot} → ${s.connection}${verified}` });
  }
  for (const slot of doctor.unbound ?? []) {
    out.push({ cls: "text-warn", text: `${slot} — unbound` });
  }
  for (const s of doctor.unknown_connection ?? []) {
    out.push({ cls: "text-fail", text: `${s.slot} → ${s.connection} — unknown connection` });
  }
  for (const slot of doctor.stray_bindings ?? []) {
    out.push({ cls: "text-warn", text: `${slot} — bound but not declared` });
  }
  if (doctor.connection_check) {
    out.push({ cls: "text-ink-400", text: `connection check ${doctor.connection_check}` });
  }
  return out;
}
