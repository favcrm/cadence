import type { AppChat } from "./chat/contract";
import type { Installation } from "../workspace-apps/workspaceApps";
import { screenTag } from "../workspace-apps/screen/screenProjection";

/** The shell screen id the chat descriptor is matched against. Social
 *  Content's one private screen answers to its declared chat context (the
 *  screen tag), so the descriptor's static prompts show above the composer;
 *  an unverified installation matches nothing. Pure and unit-tested. */
export function chatScreenFor(
  installation: Installation | null,
  verified: boolean,
  crmSection: string,
  view: string,
): string | null {
  if (!verified || installation === null) return null;
  if (installation.name === "social-content") return screenTag(installation);
  return installation.name === "crm" ? crmSection : view;
}

/** The chip and quick prompts for the route's screen (prompts only fill the composer). */
export function chatContext(
  descriptor: AppChat | null,
  screen: string | null,
  recordOpen: boolean,
): { label: string; prompts: string[] } | null {
  const entry = screen === null ? undefined : descriptor?.contexts.find((c) => c.id === screen);
  if (!entry) return null;
  return recordOpen && entry.record
    ? { label: `${entry.record.label} (open)`, prompts: entry.record.prompts }
    : { label: entry.label, prompts: entry.prompts };
}
