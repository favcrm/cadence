import type { AppChat } from "./chat/contract";
import type { Installation } from "../workspace-apps/workspaceApps";
import { screenTag } from "../workspace-apps/screen/screenProjection";
import { isSocialContent } from "../workspace-apps/socialContentSupport";

/** The shell screen id the chat descriptor is matched against: the mounted
 *  screen's declared section, else the app's one declared screen tag. Social
 *  Content without an eligible screen matches nothing (legacy). An unverified
 *  installation matches nothing. Pure and unit-tested. */
export function chatScreenFor(
  installation: Installation | null,
  verified: boolean,
  section: string | null,
  view: string,
): string | null {
  if (!verified || installation === null) return null;
  if (section !== null) return section;
  const tag = screenTag(installation);
  if (tag !== null) return tag;
  return isSocialContent(installation) ? null : view;
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
