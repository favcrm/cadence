/**
 * CAD-2657: the board chrome around a full-width canvas. While the Home canvas
 * is expanded the sidebar folds to its icon rail and the chat to a rail of its
 * own; the chat opens as a peek over the canvas. Nothing here is persisted: the
 * person's own sidebar preference returns when the canvas steps back.
 */
import { useSyncExternalStore } from "react";

export interface CanvasChrome {
  /** The canvas covers the chat column (Home surface, desktop). */
  full: boolean;
  /** The chat is open as a drawer over the canvas. */
  peek: boolean;
  /** The assistant spoke while the chat was out of sight. */
  unread: boolean;
}

const OFF: CanvasChrome = { full: false, peek: false, unread: false };
let state = OFF;
const listeners = new Set<() => void>();

function set(next: CanvasChrome): void {
  if (next.full === state.full && next.peek === state.peek && next.unread === state.unread) return;
  state = next;
  for (const listener of [...listeners]) listener();
}

export const canvasChrome = (): CanvasChrome => state;

/** Entering resets the peek and the dot; leaving restores the board as it was. */
export function setCanvasFull(full: boolean): void {
  set(full ? { ...state, full } : OFF);
}

/** Opening the chat reads it. */
export function setChatPeek(peek: boolean): void {
  set({ ...state, peek, unread: peek ? false : state.unread });
}

/** A chat host reports newer messages; only a chat that is hidden turns it into a dot. */
export function noteChatActivity(): void {
  if (state.full && !state.peek) set({ ...state, unread: true });
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}

export function useCanvasChrome(): CanvasChrome {
  return useSyncExternalStore(subscribe, canvasChrome, canvasChrome);
}
