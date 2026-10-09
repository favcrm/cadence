import { useSyncExternalStore } from "react";

/**
 * What the operator did to To do cards before the server's overview
 * catches up (CAD-1273): hidden (snoozed or dismissed), decided, and
 * "Fix it" sent. One store outside the rail so the sidebar badge counts
 * the same cards the list shows, and a slide-over that closes and
 * reopens keeps a sent card sent instead of offering "Fix it" again.
 */
export interface TodoLocal {
  hidden: ReadonlySet<string>;
  done: ReadonlyMap<string, string>;
  sent: ReadonlySet<string>;
}

let state: TodoLocal = { hidden: new Set(), done: new Map(), sent: new Set() };
const listeners = new Set<() => void>();

function set(next: TodoLocal) {
  state = next;
  for (const fn of listeners) fn();
}

export const hideTodo = (key: string) => set({ ...state, hidden: new Set(state.hidden).add(key) });
export const doneTodo = (key: string, text: string) => set({ ...state, done: new Map(state.done).set(key, text) });
export const sentTodo = (key: string) => set({ ...state, sent: new Set(state.sent).add(key) });

/** Forget what the server no longer lists, so a returning card starts fresh. */
export function reconcileTodo(present: readonly string[]) {
  const keep = new Set(present);
  const kept = <T>(keys: Iterable<T>) => [...keys].filter((k) => keep.has(String(k)));
  const hidden = kept(state.hidden);
  const done = kept(state.done.keys());
  const sent = kept(state.sent);
  if (hidden.length === state.hidden.size && done.length === state.done.size && sent.length === state.sent.size) return;
  set({
    hidden: new Set(hidden),
    done: new Map(done.map((k) => [k, state.done.get(k)!])),
    sent: new Set(sent),
  });
}

/** Clears everything — for tests, which share one module. */
export const resetTodoLocal = () => set({ hidden: new Set(), done: new Map(), sent: new Set() });

export function useTodoLocal(): TodoLocal {
  return useSyncExternalStore(
    (fn) => {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    () => state,
  );
}
