import type {
  AppDetail,
  AppDoctor,
  AppPendingSend,
  AppRow,
  AppRun,
  AppRunOutput,
  AppWorkflow,
  WorkflowRow,
} from "../../lib/types";
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
 *
 * CAD-563 r2 adds the app page's plain-language layer: the purpose
 * line, the primary action and its label, the run stage rows, the team
 * a new run starts with (the last run's owners), the slug a topic
 * suggests and the Needs-you rows.
 */
export type GateRow = Pick<AppRow, "error" | "approval" | "connections" | "source" | "project" | "name">;

/** `/apps/<project>/<name>` — one app's detail route. */
export function appHref(project: string, name: string): string {
  return `/apps/${encodeURIComponent(project)}/${encodeURIComponent(name)}`;
}

/**
 * The app page with its New-run drawer asked for — what an Apps card's
 * primary action links to (CAD-563 r2). `wf` is the app-qualified
 * workflow name.
 */
export function newRunHref(project: string, name: string, wf: string): string {
  return `${appHref(project, name)}?new=${encodeURIComponent(wf)}`;
}

/**
 * The app's one-line purpose: its `summary:`, else its title. Plain
 * words, never a slug or a digest.
 */
export function appPurpose(row: Pick<AppRow, "summary" | "title" | "name">): string {
  return row.summary?.trim() || row.title?.trim() || row.name || "App";
}

/** The workflow a run starts from, and the action's label. */
export function primaryAction(app: AppDetail): { wf: AppWorkflow; label: string } | null {
  const wf = (app.workflows ?? [])[0];
  if (!wf) return null;
  return { wf, label: wf.label?.trim() || "New run" };
}

/** A step's plain label: "Brief: my topic" → "Brief". */
export function stepLabel(title: string): string {
  const text = title.trim();
  const head = text.split(":")[0]?.trim() ?? "";
  return head !== "" && head.length < text.length ? head : text;
}

/** One stage of a run's progress row. */
export interface RunStage {
  label: string;
  /** ok = finished, run = in flight, wait = needs the operator, off = not yet. */
  tone: "ok" | "run" | "wait" | "off";
}

/**
 * A run's stage row: the workflow's steps (the run's own tickets, so
 * their states are the tracker's) and the output marker — Published
 * once an item exists, Ready to publish while a staged send waits for
 * the operator's release.
 */
export function runStages(
  run: AppRun,
  outputs: AppRunOutput[],
  pending: AppPendingSend[],
): RunStage[] {
  const stages: RunStage[] = run.plan.tickets.map((t) => ({
    label: stepLabel(t.title),
    tone:
      t.status === "done"
        ? "ok"
        : t.status === "doing" || t.status === "review"
          ? "run"
          : (t.blocked_by ?? []).length > 0
            ? "wait"
            : "off",
  }));
  if (outputs.length > 0) stages.push({ label: "Published", tone: "ok" });
  else if (pending.length > 0) stages.push({ label: "Ready to publish", tone: "wait" });
  return stages;
}

/** The published items and the staged sends of one run. */
export function outputsOf(
  run: AppRun,
  items: AppRunOutput[],
  pending: AppPendingSend[],
): { items: AppRunOutput[]; pending: AppPendingSend[] } {
  return {
    items: items.filter((i) => (i.runs ?? []).includes(run.epic)),
    pending: pending.filter((p) => p.runs.includes(run.epic)),
  };
}

/** The Posts tab's filters — one at a time, with no run falling through. */
export type RunFilter = "in_progress" | "needs_you" | "published";

/** Which filter a run shows under; `in_progress` is everything not yet published. */
export function runFilter(run: AppRun, needs: HomeNeed[], published: boolean): RunFilter {
  if (published) return "published";
  if (runState(run, needs).needsYou) return "needs_you";
  return "in_progress";
}

/** The issue a Needs-you row is about, when one is named (CAD-563): the
 *  action's own issue for plan/question/merge rows, else the subject
 *  when it is an issue (`review_no_pr` and friends). A `report` subject
 *  (`D-2/q.md`) is not an issue id, so it is not matched. */
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

/** The Needs-you rows about one run — the epic itself or one of its tickets. */
export function runNeeds(run: AppRun, needs: HomeNeed[]): HomeNeed[] {
  const tickets = new Set(run.plan.tickets.map((t) => t.id));
  return needs.filter((n) => {
    const issue = needIssue(n);
    return issue !== null && (issue === run.epic || tickets.has(issue));
  });
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
  const needsYou = runNeeds(run, needs).length > 0;
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

/** What the Apps list card says under the title: "2 in progress · 1 needs you". */
export function runsSummary(runs: AppRun[], needs: HomeNeed[]): string | null {
  if (runs.length === 0) return null;
  let progress = 0;
  let waiting = 0;
  for (const run of runs) {
    const state = runState(run, needs);
    if (state.needsYou) waiting += 1;
    else if (state.text === "running") progress += 1;
  }
  const parts: string[] = [];
  if (progress > 0) parts.push(`${progress} in progress`);
  if (waiting > 0) parts.push(`${waiting} need${waiting === 1 ? "s" : ""} you`);
  return parts.length > 0 ? parts.join(" · ") : "Nothing running";
}

/** The team a new run starts with: the last run's owners, by step. */
export function teamFromLastRun(wf: AppWorkflow, runs: AppRun[]): Record<string, string> {
  const steps = wf.steps ?? [];
  const inputs = new Set((wf.inputs ?? []).map((i) => i.name));
  const team = new Set(
    steps.map((s) => s.agent).filter((a): a is string => !!a && inputs.has(a)),
  );
  for (const run of [...runs].reverse()) {
    const out: Record<string, string> = {};
    run.plan.tickets.forEach((ticket, i) => {
      const input = steps[i]?.agent;
      if (input && team.has(input) && ticket.owner) out[input] = ticket.owner;
    });
    if (Object.keys(out).length > 0) return out;
  }
  return {};
}

/** The workflow's team inputs — the ones its steps name as their agent. */
export function teamInputs(wf: AppWorkflow): string[] {
  const inputs = new Set((wf.inputs ?? []).map((i) => i.name));
  return [
    ...new Set(
      (wf.steps ?? [])
        .map((s) => s.agent)
        .filter((a): a is string => !!a && inputs.has(a)),
    ),
  ];
}

/** The slug a topic suggests: lowercase words joined by hyphens. */
export function slugFromTopic(topic: string): string {
  return topic
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 60)
    .replace(/-+$/g, "");
}

/** Where a slot publishes, in words. */
export function publishTarget(app: AppDetail, slot: string): string {
  const bound = (app.connections ?? []).find((c) => c.slot === slot)?.bound;
  if (bound == null) return "not connected yet";
  return bound === "local" ? "Local outbox" : bound;
}

/** The app's slots its workflows actually use, in declaration order. */
export function usedSlots(app: AppDetail): string[] {
  const used = new Set((app.workflows ?? []).flatMap((wf) => wf.uses ?? []));
  return (app.connections ?? []).map((c) => c.slot).filter((s) => used.has(s));
}

/** The kept-apart inputs, in words, or null when the workflow has none. */
export function distinctNote(wf: AppWorkflow): string | null {
  const names = (wf.distinct ?? []).filter((n) => n !== "");
  if (names.length < 2) return null;
  return `Kept apart: ${names.join(", ")}.`;
}

/** One step of the "How it works" list, in plain words. */
export interface StepRow {
  label: string;
  /** Who works it — the last run's agent when known. */
  who: string | null;
}

/** The workflow's steps with who works each, from the last run's team. */
export function stepRows(wf: AppWorkflow, team: Record<string, string>): StepRow[] {
  const inputs = new Set((wf.inputs ?? []).map((i) => i.name));
  return (wf.steps ?? []).map((s) => ({
    label: stepLabel(s.title),
    who: s.agent ? (inputs.has(s.agent) ? (team[s.agent] ?? null) : s.agent) : null,
  }));
}

/** The app page's Needs-you rows, in the operator's order. */
export interface AppNeed {
  kind: "approve" | "release" | "question";
  text: string;
  /** The run the row is about, when one is. */
  run?: string;
}

/**
 * What the app page's Needs-you strip holds: an app whose contents moved
 * since approval, the sends staged for release, and the questions the
 * runs raised. Empty when nothing waits on the operator.
 */
export function appNeeds(
  app: AppDetail,
  runs: AppRun[],
  needs: HomeNeed[],
  pending: AppPendingSend[],
): AppNeed[] {
  const out: AppNeed[] = [];
  if (approvalPending(app)) {
    out.push({ kind: "approve", text: `Approve ${app.name} — its contents changed` });
  }
  for (const run of runs) {
    for (const send of outputsOf(run, [], pending).pending) {
      out.push({
        kind: "release",
        text: `${send.title?.trim() || run.title} — ready to publish`,
        run: run.epic,
      });
    }
    for (const need of runNeeds(run, needs)) {
      if (need.kind === "question" || need.action.type === "answer") {
        out.push({ kind: "question", text: need.title, run: run.epic });
      }
    }
  }
  return out;
}

/** `/outbox?item=<effect_id>` — one published item's detail (CAD-546). */
export function outboxHref(effectId: string): string {
  return `/outbox?item=${encodeURIComponent(effectId)}`;
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

/**
 * The doctor row's problems, flattened for the detail: the slots that
 * resolve are the connection list's own rows (shown once, with their
 * verification), so only the findings that need the operator land
 * here — unbound, unknown, stray, and the connection check's state.
 */
export function doctorFindings(doctor: AppDoctor | null | undefined): { cls: string; text: string }[] {
  if (!doctor) return [];
  const out: { cls: string; text: string }[] = [];
  for (const s of doctor.slots_ok ?? []) {
    if (s.verified) out.push({ cls: "text-ok", text: `${s.slot} → ${s.connection} — verification ${s.verified}` });
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

/** The connection rows the detail shows: every declared slot, once. */
export function connectionRows(doctor: AppDoctor | null | undefined): { cls: string; text: string }[] {
  if (!doctor) return [];
  return (doctor.slots_ok ?? []).map((s) => ({
    cls: "text-ok",
    text: `${s.slot} → ${s.connection}`,
  }));
}
