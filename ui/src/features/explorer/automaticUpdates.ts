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

/** The git URL this install was recorded from. A path typed into the
 *  update field is not this, and a retired built-in source has none. */
export function recordedPackageSource(
  source: { kind?: string; url?: string | null } | null | undefined,
): string {
  if (source?.kind !== "git" || typeof source.url !== "string") return "";
  return source.url.trim();
}

/** What an automatic apply tells the server it saw. The server re-reads
 *  this at commit, after the package fetch, and refuses the upgrade when
 *  the install is no longer approved or the switch is off. A manual Apply
 *  omits the pair; that click is itself the consent. */
export function automaticUpgradeClaim(): { automatic: true; observed_approval: "approved" } {
  return { automatic: true, observed_approval: "approved" };
}

/** Automatic apply is not consent. A removed, disabled, or
 *  not-yet-approved install stays off until someone approves it. */
export function installConsentAllowsAutomatic(install: {
  approved?: boolean | null;
  removed?: number | null;
  state?: string | null;
  approval?: { state?: string } | null;
} | null | undefined): boolean {
  if (!install || install.removed != null) return false;
  if (install.state === "disabled" || install.state === "removed") return false;
  return install.approved === true || install.approval?.state === "approved";
}

/** Automatic apply is only a routine receipt of the recorded upstream.
 *  Any other source, including one the operator typed, stays on Apply. */
export function mayApplyAutomatically(input: {
  checkedSource: string;
  recordedSource: string;
  receipt: Parameters<typeof routineRelease>[0];
}): boolean {
  const checked = input.checkedSource.trim();
  const recorded = input.recordedSource.trim();
  if (!recorded || checked !== recorded) return false;
  return routineRelease(input.receipt);
}

/** Declared connections or capabilities differ. An unreadable diff is
 *  treated as a reach change so it cannot apply on its own. */
export function releaseChangesReach(diff: AccessDiff | undefined): boolean {
  if (!diff?.connections || !diff.capabilities) return true;
  if ((diff.connections.added?.length ?? 0) > 0) return true;
  if ((diff.connections.removed?.length ?? 0) > 0) return true;
  return Object.keys(diff.capabilities).length > 0;
}
