import type { PolicyDraft, RecordedVisibility } from "./policyContract";

/** The recorded spelling, which is also the CLI flag's spelling. */
export type Visibility = RecordedVisibility;

export type { PolicyDraft };

/**
 * The CLI command for exactly this state (CAD-1950).
 *
 * `--grant` is the full list and omitting it clears, so grants are always
 * spelled out — including on a workspace-visible project, where they are
 * stored but inert. Printing them is what keeps the line and the save button
 * describing one state instead of two. Hosted grants are member subject ids;
 * the same flag carries them unchanged.
 */
export function cliCommand(project: string, draft: PolicyDraft): string {
  const grants = draft.grants.map((member) => ` --grant ${member}`).join("");
  return `cadence issue project policy ${project} --visibility ${draft.visibility.replace("_", "-")}${grants}`;
}
