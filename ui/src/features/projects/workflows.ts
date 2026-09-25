import type { WorkflowRow } from "../../lib/types";
import type { Viewer } from "./work";

/**
 * The Workflows section's view of a stored workflow (CAD-496) — the
 * adapters between the `/api/projects/<key>/workflows` rows and what the
 * run form needs, kept pure so the rules are unit-tested in plain node
 * (tests/workflows.test.ts).
 */

/** One field of the run form, generated from the workflow's `inputs:`. */
export interface RunField {
  name: string;
  /** What the operator is asked — the frontmatter `ask`, else the name. */
  label: string;
  optional: boolean;
}

/** The run form's fields, straight from the workflow's frontmatter inputs. */
export function runFields(row: WorkflowRow): RunField[] {
  return (row.inputs ?? []).map((input) => ({
    name: input.name,
    label: input.ask?.trim() ? input.ask : input.name,
    optional: input.optional === true,
  }));
}

/**
 * The `inputs` map preview and propose send: declared names only, each
 * trimmed, empty values dropped — an absent required input is the
 * daemon's named refusal, an absent optional renders empty.
 */
export function providedInputs(
  row: WorkflowRow,
  values: Record<string, string>,
): Record<string, string> {
  const out: Record<string, string> = {};
  for (const input of row.inputs ?? []) {
    const value = (values[input.name] ?? "").trim();
    if (value !== "") out[input.name] = value;
  }
  return out;
}

/** Required inputs the form has no value for — Propose waits on them. */
export function missingRequired(
  row: WorkflowRow,
  values: Record<string, string>,
): string[] {
  return (row.inputs ?? [])
    .filter((input) => input.optional !== true && !(values[input.name] ?? "").trim())
    .map((input) => input.name);
}

/**
 * Why this workflow cannot be proposed at all — a broken file, or the
 * gate approval `plan_propose` checks. `null` means the daemon's gate
 * would let it through.
 */
export function gateBlock(row: WorkflowRow): string | null {
  if (row.error) return row.error;
  if (row.approved === true) return null;
  if (typeof row.approved === "string" && row.approved) {
    return `approval state ${row.approved}`;
  }
  return `not approved for its current gate keys — approve it with \`cadence workflow approve ${row.name} --project ${row.project}\``;
}

/**
 * Why the Propose button is disabled, checked in the order an operator
 * meets them: the board's own write gate, the workflow's gate, the
 * form's required inputs, then whether the board can prove the operator
 * to the daemon. `null` — a run may be proposed.
 */
export function proposeBlock(
  row: WorkflowRow,
  viewer: Viewer,
  values: Record<string, string>,
): string | null {
  if (viewer.readOnly) return "The board is read-only — sign in as the operator to propose a run.";
  const gate = gateBlock(row);
  if (gate) return gate;
  const missing = missingRequired(row, values);
  if (missing.length > 0) {
    return `missing required ${missing.length > 1 ? "inputs" : "input"}: ${missing.join(", ")}`;
  }
  if (!viewer.operator) {
    return "Proposing a run is the operator's — this board cannot prove the operator to the daemon. `cadence plan propose --workflow` does it from the shell.";
  }
  return null;
}

/** The approval chip for a workflow row — a static class per state. */
export function approvalChip(row: WorkflowRow): { text: string; cls: string } {
  if (row.error) return { text: "broken", cls: "bg-fail/10 text-fail" };
  if (row.approved === true) return { text: "approved", cls: "bg-ok/15 text-ok" };
  if (typeof row.approved === "string") return { text: "approval unknown", cls: "bg-warn/10 text-warn" };
  return { text: "unapproved", cls: "bg-warn/10 text-warn" };
}

/**
 * A daemon refusal shown by name — the code (`one_line`,
 * `not_distinct`, `render_diverged`, `workflow_unapproved`) prefixed
 * to its reason, `code: reason`. No code reads as the bare reason.
 */
export function refusalText(
  code: string | null | undefined,
  reason: string | null | undefined,
): string {
  const text = reason?.trim() ? reason : "refused";
  return code ? `${code}: ${text}` : text;
}

/** What a propose answer carries for the UI — the new plan epic. */
export function proposedEpic(out: unknown): { epic: string; title: string; tickets: number } | null {
  const o = out as { epic?: unknown; title?: unknown; tickets?: unknown } | null;
  const epic = typeof o?.epic === "string" ? o.epic : null;
  if (!epic) return null;
  return {
    epic,
    title: typeof o?.title === "string" ? o.title : "",
    tickets: Array.isArray(o?.tickets) ? o.tickets.length : 0,
  };
}
