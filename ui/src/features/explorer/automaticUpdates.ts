import type { AccessDiff } from "../workspace-apps/workspaceApps";

/** A checked release the board may apply without another confirmation.
 *  Missing access, a secret warning, or a failed compatibility report is
 *  not routine: the existing Apply stays the confirmation. */
export function routineRelease(proposal: {
  access_diff?: AccessDiff;
  secret_warnings?: unknown[] | null;
  compatibility?: { ok?: boolean | null; compatible?: boolean | null } | null;
}): boolean {
  if (releaseChangesReach(proposal.access_diff)) return false;
  if (!Array.isArray(proposal.secret_warnings) || proposal.secret_warnings.length > 0) return false;
  if (proposal.compatibility?.ok !== true || proposal.compatibility.compatible !== true) return false;
  return true;
}

/** Declared connections or capabilities differ. An unreadable diff is
 *  treated as a reach change so it cannot apply on its own. */
export function releaseChangesReach(diff: AccessDiff | undefined): boolean {
  if (!diff?.connections || !diff.capabilities) return true;
  if ((diff.connections.added?.length ?? 0) > 0) return true;
  if ((diff.connections.removed?.length ?? 0) > 0) return true;
  return Object.keys(diff.capabilities).length > 0;
}
