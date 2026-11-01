import type { ProjectPolicy } from "../../lib/types";

/** The recorded spelling, which is also the CLI flag's spelling. */
export type Visibility = ProjectPolicy["visibility"];

export interface PolicyDraft {
  visibility: Visibility;
  members: string[];
}

/**
 * The CLI command for exactly this state (CAD-1950).
 *
 * `--grant` is the full list and omitting it clears, so grants are always
 * spelled out — including on a workspace-visible project, where they are
 * stored but inert. Printing them is what keeps the line and the save button
 * describing one state instead of two.
 */
export function cliCommand(project: string, draft: PolicyDraft): string {
  const grants = draft.members.map((member) => ` --grant ${member}`).join("");
  return `cadence issue project policy ${project} --visibility ${draft.visibility.replace("_", "-")}${grants}`;
}

/**
 * Does the draft differ from the record? A project with **no** policy is
 * always a change: recording workspace-visible is a write, and without it the
 * page could never create the policy it invites the operator to create.
 */
export function policyDirty(
  recorded: boolean,
  base: PolicyDraft | null,
  draft: PolicyDraft | null,
): boolean {
  if (!draft || !base) return false;
  if (!recorded) return true;
  return (
    draft.visibility !== base.visibility ||
    draft.members.length !== base.members.length ||
    draft.members.some((member, index) => member !== base.members[index])
  );
}
