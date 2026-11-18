import { createContext } from "react";

/**
 * CAD-2458: a card's row action fills the chat composer with a prompt and
 * never sends it. The owner of the composer (Home, an app pane) provides the
 * fill; with no provider, or a composer that cannot send, the actions are not
 * drawn.
 */
export const ComposerFillContext = createContext<((text: string) => void) | null>(null);

/** Appends a prompt to any unsent draft instead of replacing it. */
export function appendComposerDraft(existing: string, text: string): string {
  return existing.trim() ? `${existing.trimEnd()}\n${text}` : text;
}
