import type { AppDoctor, AppRow } from "../../lib/types";
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
 * `?run=<name>` on the project's Workflows screen opens that row's New
 * run form — `<app>/<wf>` names ride along percent-encoded, so the
 * existing CAD-496 form runs app workflows untouched.
 */
export function runHref(project: string, workflow: string): string {
  return `/projects/${encodeURIComponent(project)}/workflows?run=${encodeURIComponent(workflow)}`;
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
