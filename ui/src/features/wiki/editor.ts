import type { StorageLike } from "../../lib/draft";
import { WikiError } from "./api";

/**
 * The editor's two state machines (CAD-581): the sessionStorage draft and
 * the `if_rev` conflict. Both are pure over their inputs, so the conflict
 * path — save, be refused, keep the text — is unit-tested without a
 * browser (tests/wikiEditor.test.ts).
 */

export interface WikiDraft {
  path: string;
  text: string;
  /** The rev the text was edited from; the save sends it as `if_rev`. */
  baseRev: string;
  at: number;
}

const DRAFT_PREFIX = "cadence.wiki.draft:";

export function draftKey(path: string): string {
  return `${DRAFT_PREFIX}${path}`;
}

/** Stash the working text; a draft equal to the base is dropped instead. */
export function stashDraft(storage: StorageLike, draft: WikiDraft): void {
  try {
    if (draft.text) storage.setItem(draftKey(draft.path), JSON.stringify(draft));
    else storage.removeItem(draftKey(draft.path));
  } catch {
    // Storage blocked or full — the edit still lives in the textarea.
  }
}

export function readDraft(storage: StorageLike, path: string): WikiDraft | null {
  try {
    const raw = storage.getItem(draftKey(path));
    if (!raw) return null;
    const parsed = JSON.parse(raw) as WikiDraft;
    return parsed && typeof parsed.text === "string" && typeof parsed.path === "string" ? parsed : null;
  } catch {
    return null;
  }
}

export function dropDraft(storage: StorageLike, path: string): void {
  try {
    storage.removeItem(draftKey(path));
  } catch {
    // Nothing to do — a draft that cannot be removed is still overwritten
    // by the next stash, and the save path clears it again.
  }
}

export interface ConflictInfo {
  rev: string;
  author?: string | null;
  at?: string | null;
}

/** The conflict a refused save carries: the rev that won, and who wrote it. */
export function conflictFrom(error: unknown): ConflictInfo | null {
  if (!(error instanceof WikiError)) return null;
  if (error.status !== 409 && error.code !== "conflict") return null;
  return { rev: error.rev ?? "?", author: error.author, at: error.at };
}

/** The banner line, e.g. `changed since you opened it — master saved rev a41f9c2`. */
export function conflictText(conflict: ConflictInfo): string {
  const who = conflict.author ? `${conflict.author} saved` : "saved";
  return `changed since you opened it — ${who} rev ${conflict.rev}`;
}

/** The save request's body: the text plus the rev it was edited from. */
export function saveBody(path: string, text: string, baseRev: string) {
  return { path, text, if_rev: baseRev };
}
