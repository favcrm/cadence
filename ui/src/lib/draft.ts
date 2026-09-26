/**
 * The composer draft across a build-mismatch reload (CAD-573): the
 * banner's Reload click stashes the textarea's text in sessionStorage,
 * and the next page load writes it back. Every step swallows storage
 * failures — a private-mode tab still reloads, just without the draft.
 */

const KEY = "cadence.reload.composerDraft";

export type StorageLike = Pick<Storage, "getItem" | "setItem" | "removeItem">;

/** sessionStorage when reachable — even the getter can throw. */
export function sessionStore(): Storage | undefined {
  try {
    return window.sessionStorage;
  } catch {
    return undefined;
  }
}

/** The composer's textarea on the page, when one is mounted. */
export function composerField(
  doc: Pick<Document, "querySelector">,
): HTMLTextAreaElement | null {
  return doc.querySelector<HTMLTextAreaElement>("[data-composer] textarea");
}

/** Stash what the composer holds now; null or empty clears a stale stash. */
export function stashDraft(storage: StorageLike, draft: string | null | undefined): void {
  try {
    if (draft) storage.setItem(KEY, draft);
    else storage.removeItem(KEY);
  } catch {
    // Storage unavailable or full — reload anyway, draft lost.
  }
}

/** Read the stashed draft once and clear it; null when none or blocked. */
export function takeDraft(storage: StorageLike): string | null {
  try {
    const draft = storage.getItem(KEY);
    storage.removeItem(KEY);
    return draft || null;
  } catch {
    return null;
  }
}

/**
 * Write a draft into a live composer field so React owns it: the
 * prototype setter updates the DOM value past React's value tracker,
 * then a bubbling `input` event runs its onChange.
 */
export function applyDraft(field: HTMLTextAreaElement, draft: string): void {
  const setter = Object.getOwnPropertyDescriptor(
    HTMLTextAreaElement.prototype,
    "value",
  )?.set;
  if (setter) setter.call(field, draft);
  else field.value = draft;
  field.dispatchEvent(new Event("input", { bubbles: true }));
}
