import type { ResourceState } from "../../lib/cache";
import { readLifecycle } from "./archiveContract";

/** Why project work is read-only when the project is archived. */
export const ARCHIVED_WRITE_BLOCK =
  "This project is archived — restore it before changing its work.";
/** Why project work is read-only while the archive state is not current. */
export const UNVERIFIED_WRITE_BLOCK =
  "This project's archive state could not be verified, so its work stays read-only until it answers.";

/**
 * CAD-2255: the write gate for one project's work (an issue drawer, a direct
 * issue page).
 *
 * Only a lifecycle read that is CURRENT (no refresh in flight, last read
 * succeeded), names `project`, and proves it ACTIVE unlocks. A cached active
 * receipt is not enough while revalidating — another tab may have archived
 * the project since that answer. Unknown, unavailable, stale, failed,
 * malformed or archived all block.
 */
function projectArchiveBlock(project: string, state: ResourceState<unknown> | null): string | null {
  const current =
    state !== null &&
    !state.inFlight &&
    (state.status === "ok" || state.status === "empty");
  if (current) {
    const view = readLifecycle(state.data, project);
    if (view.kind === "available") {
      return view.receipt.archived ? ARCHIVED_WRITE_BLOCK : null;
    }
  }
  return UNVERIFIED_WRITE_BLOCK;
}

/**
 * The issue drawer's write gate. The project must come from the item itself
 * (loaded detail or its card) — the route a drawer was opened from is never
 * proof, because it can show an issue from another project.
 */
export function drawerArchiveGate({
  project,
  state,
}: {
  project: string | null;
  state: ResourceState<unknown> | null;
}): { writeBlock: string | null } {
  return {
    writeBlock:
      project === null
        ? "This issue's project could not be determined, so its work stays read-only."
        : projectArchiveBlock(project, state),
  };
}

/**
 * The direct issue page's write gate. The loaded issue's own project must
 * match the requested route project; an unloaded or mismatched item fails
 * closed rather than borrowing the route key's authority.
 */
export function issuePageWriteBlock({
  routeProject,
  detailProject,
  state,
}: {
  routeProject: string;
  /** `detail.project` from the loaded issue, or null before it loads. */
  detailProject: string | null;
  state: ResourceState<unknown> | null;
}): string | null {
  if (detailProject === null) {
    return "This issue's project has not loaded, so its work stays read-only.";
  }
  if (detailProject !== routeProject) {
    return "This issue belongs to a different project than this page requested, so its work stays read-only.";
  }
  return projectArchiveBlock(detailProject, state);
}
