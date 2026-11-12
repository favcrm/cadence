/**
 * CAD-2255: the project-archive lifecycle contract, read as what it proves.
 *
 * `GET /api/projects/:key/lifecycle`, `POST …/archive` and `POST …/restore`
 * all answer one receipt:
 *
 *   {archive_capability: "cad2255.project-archive.v1", project, archived,
 *    revision, archived_at, archived_by, changed_at, changed_by, blockers}
 *
 * Nothing here defaults a capability on. A server that does not answer the
 * route (404, a transient failure) is `unknown`, another capability is
 * `unavailable`, and a shape this module cannot prove is `unreadable` —
 * never "active".
 *
 * `archived_at`/`archived_by` are last-archive evidence and stay after a
 * restore; only `archived` is current state. The receipt's own invariants
 * are enforced: revision 0 is an untouched project — active with every
 * evidence field null; revision > 0 names both the last archive and the
 * last change (archive OR restore) with non-empty actors and valid ISO
 * timestamps. A receipt that breaks these cannot come from the server. */

import { objectField } from "../../lib/wire";

/** The one capability this page knows how to drive. */
export const ARCHIVE_CAPABILITY = "cad2255.project-archive.v1";

export interface ArchiveReceipt {
  archive_capability: typeof ARCHIVE_CAPABILITY;
  project: string;
  /** Current state. `archived_at`/`archived_by` are last-archive evidence. */
  archived: boolean;
  /** The fence a transition pins; a later receipt carries a larger number. */
  revision: number;
  archived_at: string | null;
  archived_by: string | null;
  /** The last archive OR restore. */
  changed_at: string | null;
  changed_by: string | null;
  /** Reasons the server refuses the archive transition right now. */
  blockers: string[];
}

/**
 * What the settings page may do with policy mutation, from the receipt.
 * Only `active` unlocks mutation: `archived` is read-only, and `unknown` /
 * `unavailable` are unverified — a missing endpoint does not prove the
 * storage never held an archive, so both fail closed.
 */
export type ArchiveGateState = "archived" | "active" | "unknown" | "unavailable";

export type ArchiveView =
  /** The board proved the capability and answered a shaped receipt. */
  | { kind: "available"; receipt: ArchiveReceipt }
  /** The route or capability is positively proved absent; archive is not offered. */
  | { kind: "unavailable"; reason: string }
  /**
   * The board did not prove the state: an unanswered route, a transient
   * failure, or an answer that names no capability. Policy mutation stays
   * locked on this state — unknown is never treated as active.
   */
  | { kind: "unknown"; reason: string }
  /** The server answered, but not with a lifecycle this page can trust. */
  | { kind: "unreadable"; reason: string };

/** RFC 3339 as the server emits it: a date, time and explicit zone. */
const ISO_TIMESTAMP = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d{1,9})?(?:Z|[+-]\d{2}:\d{2})$/;

function timestamp(value: unknown): string | null | undefined {
  if (value === null) return null;
  if (typeof value !== "string" || !ISO_TIMESTAMP.test(value) || Number.isNaN(Date.parse(value))) {
    return undefined;
  }
  return value;
}

function actor(value: unknown): string | null | undefined {
  if (value === null) return null;
  return typeof value === "string" && value.trim() !== "" ? value : undefined;
}

function blockersOf(value: unknown): string[] | null {
  if (!Array.isArray(value)) return null;
  const out: string[] = [];
  for (const entry of value) {
    if (typeof entry !== "string" || entry.trim() === "") return null;
    out.push(entry);
  }
  return out;
}

/**
 * Read one lifecycle/receipt answer for `project`.
 *
 * A receipt naming another project is unreadable — a page must never render
 * one project's state as another's. The same applies to a receipt that does
 * not carry the current `archived` flag or a numeric revision.
 */
export function readLifecycle(raw: unknown, project: string): ArchiveView {
  const sentinel = objectField(raw);
  if (sentinel?.archive_unavailable === true) {
    // Only an explicit 501 proves this server has no archive capability. A
    // 404 (or any other unserved answer) could be an older server, a route
    // behind another gate, or a project this caller cannot see — unknown,
    // never "unavailable", and never active.
    return sentinel.status === 501
      ? { kind: "unavailable", reason: "This server does not offer project archive." }
      : { kind: "unknown", reason: "The server did not answer the archive lifecycle route." };
  }
  const body = objectField(raw);
  if (!body) return { kind: "unreadable", reason: "the lifecycle answer is not an object" };
  const capability = body.archive_capability;
  if (capability === undefined || capability === null) {
    return { kind: "unknown", reason: "This board's answer does not name an archive capability." };
  }
  if (capability !== ARCHIVE_CAPABILITY) {
    return {
      kind: "unavailable",
      reason: "This board answered with an archive capability this page does not recognise.",
    };
  }
  const named = typeof body.project === "string" && body.project.trim() !== "" ? body.project : null;
  if (named === null) return { kind: "unreadable", reason: "the receipt names no project" };
  if (named !== project) {
    return { kind: "unreadable", reason: "the receipt belongs to a different project" };
  }
  if (typeof body.archived !== "boolean") {
    return { kind: "unreadable", reason: "the receipt has no current archived state" };
  }
  const revision = body.revision;
  if (typeof revision !== "number" || !Number.isSafeInteger(revision) || revision < 0) {
    return { kind: "unreadable", reason: "the receipt has no usable revision" };
  }
  const archivedAt = timestamp(body.archived_at);
  const archivedBy = actor(body.archived_by);
  const changedAt = timestamp(body.changed_at);
  const changedBy = actor(body.changed_by);
  if (archivedAt === undefined || archivedBy === undefined || changedAt === undefined || changedBy === undefined) {
    return { kind: "unreadable", reason: "the receipt's timestamps and actors are malformed" };
  }
  if (revision === 0) {
    if (
      body.archived !== false ||
      archivedAt !== null ||
      archivedBy !== null ||
      changedAt !== null ||
      changedBy !== null
    ) {
      return {
        kind: "unreadable",
        reason: "an untouched receipt (revision 0) cannot carry an archived state or evidence",
      };
    }
  } else if (archivedAt === null || archivedBy === null || changedAt === null || changedBy === null) {
    return {
      kind: "unreadable",
      reason: "a transitioned receipt must name both its archive and last-change evidence",
    };
  }
  const blockers = blockersOf(body.blockers);
  if (blockers === null) return { kind: "unreadable", reason: "the receipt's blockers are malformed" };
  return {
    kind: "available",
    receipt: {
      archive_capability: ARCHIVE_CAPABILITY,
      project: named,
      archived: body.archived,
      revision,
      archived_at: archivedAt,
      archived_by: archivedBy,
      changed_at: changedAt,
      changed_by: changedBy,
      blockers,
    },
  };
}

/**
 * Does a write's answer prove the transition this page asked for? The
 * receipt must name this project, report the intended state, and carry
 * exactly the next revision (`expectedRevision + 1`). A jump can belong to
 * another actor's transition and is not this operation's success; a receipt
 * at or below the fence is stale. Neither is reported as success.
 */
export function receiptMatches(
  view: ArchiveView,
  intent: { project: string; archived: boolean; expectedRevision: number },
): boolean {
  if (view.kind !== "available") return false;
  if (
    !Number.isSafeInteger(intent.expectedRevision) ||
    intent.expectedRevision < 0 ||
    intent.expectedRevision + 1 > Number.MAX_SAFE_INTEGER
  ) {
    return false;
  }
  const { receipt } = view;
  return (
    receipt.project === intent.project &&
    receipt.archived === intent.archived &&
    receipt.revision === intent.expectedRevision + 1
  );
}

/* ------------------------- archived-project discovery ------------------------- */

/** One archived row of `GET /api/projects?archived=1`. */
export interface ArchivedProjectRow {
  key: string;
  prefix: string;
  components: string[];
  repos: { path?: string; remote?: string }[];
  issues: number;
  archived: true;
}

export type ArchivedProjectsView =
  | { kind: "available"; projects: ArchivedProjectRow[] }
  /** The board does not answer the archived query as a capability. */
  | { kind: "unavailable"; reason: string }
  | { kind: "unreadable"; reason: string };

/**
 * Read `GET /api/projects?archived=1`. A server that predates the route
 * ignores the query and answers the ordinary list — no `archive_supported`
 * — which is `unavailable`, never "no archived projects". Only an explicit
 * `archive_supported: true` beside rows that each say `archived: true`
 * is a proved archived view.
 */
export function readArchivedProjects(raw: unknown): ArchivedProjectsView {
  if (objectField(raw)?.archive_unavailable === true) {
    return { kind: "unavailable", reason: "This server does not offer an archived-project view." };
  }
  const body = objectField(raw);
  if (!body) return { kind: "unreadable", reason: "the archived-project answer is not an object" };
  if (body.archive_supported !== true) {
    return {
      kind: "unavailable",
      reason: "This board's server does not offer an archived-project view.",
    };
  }
  if (!Array.isArray(body.projects)) {
    return { kind: "unreadable", reason: "the archived-project answer has no project list" };
  }
  const projects: ArchivedProjectRow[] = [];
  for (const entry of body.projects) {
    const row = objectField(entry);
    if (!row) return { kind: "unreadable", reason: "an archived project row is not an object" };
    if (row.archived !== true) {
      return {
        kind: "unreadable",
        reason: "the archived list carries a row that is not archived",
      };
    }
    const key = typeof row.key === "string" && row.key.trim() !== "" ? row.key : null;
    if (key === null) return { kind: "unreadable", reason: "an archived project row has no key" };
    const prefix = typeof row.prefix === "string" ? row.prefix : null;
    if (prefix === null) return { kind: "unreadable", reason: "an archived project row has no prefix" };
    const componentsOk =
      Array.isArray(row.components) && row.components.every((value) => typeof value === "string");
    if (!componentsOk) {
      return { kind: "unreadable", reason: "an archived project row has malformed components" };
    }
    const reposOk =
      Array.isArray(row.repos) &&
      row.repos.every((value) => {
        const repo = objectField(value);
        return (
          repo !== null &&
          (repo.path === undefined || typeof repo.path === "string") &&
          (repo.remote === undefined || typeof repo.remote === "string")
        );
      });
    if (!reposOk) return { kind: "unreadable", reason: "an archived project row has malformed repos" };
    const issues = row.issues;
    if (typeof issues !== "number" || !Number.isFinite(issues)) {
      return { kind: "unreadable", reason: "an archived project row has no issue count" };
    }
    projects.push({
      key,
      prefix,
      components: row.components as string[],
      repos: row.repos as { path?: string; remote?: string }[],
      issues,
      archived: true,
    });
  }
  return { kind: "available", projects };
}
