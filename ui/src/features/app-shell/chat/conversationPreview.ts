import { useEffect, useRef, useSyncExternalStore } from "react";
import { onPrivateSessionReset, privateSessionGeneration } from "../../../lib/privateSession";
import { carriesSecret } from "./directive";

/**
 * The live assistant text a `preview` stream frame carries (CAD-2228).
 *
 * A preview is the assistant's in-flight words, and nothing else: the frame
 * has no `id`, the text never enters the thread store, and it never advances
 * the resume cursor, the reply watermark or the pending reconcile. It is
 * therefore held here — conversation-scoped, in memory, replaced frame by
 * frame — and every surface reads it through `useConversationPreview`.
 *
 * The engine that owns the stream decides its lifetime: it clears the text
 * when the subscription starts, drops it when the stream errors or the
 * conversation changes, and retires it on a committed assistant entry. An
 * older server never sends the frame, so the map stays empty and nothing in
 * the renderer changes.
 */

/** What one frame may contribute, in characters. A server (or a proxy that
 *  rewrites it) cannot grow the renderer past this. */
export const PREVIEW_TEXT_MAX = 8192;

interface PreviewEntry {
  text: string;
  /** A private-session reset invalidates the text, not only the stream. */
  generation: number;
}

const previews = new Map<string, PreviewEntry>();
const listeners = new Set<() => void>();
let notifyScheduled = false;

/** One notification per animation frame, never one per token. */
function notify(): void {
  if (notifyScheduled || listeners.size === 0) return;
  notifyScheduled = true;
  const flush = () => {
    notifyScheduled = false;
    for (const listener of [...listeners]) listener();
  };
  if (typeof requestAnimationFrame === "function") requestAnimationFrame(flush);
  else setTimeout(flush, 16);
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** The server has no live text: an SSR render (a static-markup test) paints
 *  nothing here, and hydration starts from empty rather than mismatching. */
const serverPreview = (): string | null => null;

/** The text of `conversation` as it may be shown now; null when none. */
function previewOf(conversation: string | null): string | null {
  if (conversation === null) return null;
  const entry = previews.get(conversation);
  if (!entry) return null;
  return entry.generation === privateSessionGeneration() ? entry.text : null;
}

/**
 * What of one frame may be painted. The whole-JSON confirmation block is
 * never previewed: the guard is applied again here, so a prose block that
 * runs into a `{` block shows the prose and withholds everything from the
 * block's first line on — concatenation cannot smuggle it past the check.
 * A token-bearing body is withheld whole, as it is for reasoning.
 */
function previewText(text: string): string | null {
  const bounded = text.length > PREVIEW_TEXT_MAX ? text.slice(0, PREVIEW_TEXT_MAX) : text;
  if (carriesSecret(bounded)) return null;
  let offset = 0;
  for (;;) {
    const end = bounded.indexOf("\n", offset);
    const line = bounded.slice(offset, end < 0 ? bounded.length : end);
    if (line.trimStart().startsWith("{")) {
      const kept = bounded.slice(0, offset).trimEnd();
      return kept === "" ? null : kept;
    }
    if (end < 0) break;
    offset = end + 1;
  }
  return bounded.trim() === "" ? null : bounded;
}

/** Replace `conversation`'s live text; null clears it. */
export function setConversationPreview(conversation: string | null, text: string | null): void {
  if (conversation === null) return;
  if (text === null) {
    if (!previews.delete(conversation)) return;
  } else {
    const next = previewText(text);
    if (next === null) {
      if (!previews.delete(conversation)) return;
    } else {
      if (previewOf(conversation) === next) return;
      previews.set(conversation, { text: next, generation: privateSessionGeneration() });
    }
  }
  notify();
}

/**
 * Fold the body of one `event: preview` frame (`{"text": string|null}`).
 * Unparseable or foreign bodies are ignored: a preview never paints what it
 * cannot read as text.
 */
export function applyPreviewFrame(conversation: string | null, data: string): void {
  if (conversation === null) return;
  let body: unknown;
  try {
    body = JSON.parse(data);
  } catch {
    return;
  }
  if (body === null || typeof body !== "object" || !Object.hasOwn(body, "text")) return;
  const text = "text" in body ? body.text : undefined;
  if (text === null) {
    setConversationPreview(conversation, null);
    return;
  }
  if (typeof text !== "string") return;
  setConversationPreview(conversation, text);
}

/**
 * The live assistant text for `conversation` while `active` — the surface's
 * own turn state says a turn is generating here. Leaving that turn clears the
 * text (a later turn must not open on the previous one's words), and a text
 * that arrives while no turn is generating is never painted.
 */
export function useConversationPreview(conversation: string | null, active: boolean): string | null {
  // `active` is part of the snapshot, not only of the return value: a
  // notification leaves a finished turn's snapshot (null) unchanged, so the
  // transcript does not re-render for text that is not its own — only the
  // turn that is generating sees a new value each frame.
  const text = useSyncExternalStore(
    subscribe,
    () => (active ? previewOf(conversation) : null),
    // Stable identity: a server render and hydration both paint no live text.
    serverPreview,
  );
  const shown = useRef({ conversation, active });
  useEffect(() => {
    const previous = shown.current;
    shown.current = { conversation, active };
    if (previous.conversation !== conversation) {
      // A conversation switch retires the text the other one was painting.
      if (previous.active) setConversationPreview(previous.conversation, null);
      return;
    }
    if (previous.active && !active) setConversationPreview(conversation, null);
  }, [active, conversation]);
  return text;
}

// A reset abandons every private observation, live text included.
onPrivateSessionReset(() => {
  if (previews.size === 0) return;
  previews.clear();
  notify();
});
