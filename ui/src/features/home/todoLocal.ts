import { useEffect, useSyncExternalStore } from "react";
import type { NeedsMe } from "../../lib/types";
import { needFingerprint, todoCount, todoSplit, type HomeNeed } from "./needs";

/**
 * What the operator did to To do cards before the server's overview
 * catches up (CAD-1273): hidden (snoozed or dismissed), decided, "Fix it"
 * sending and sent. One store outside the rail so the sidebar badge counts
 * the same cards the list shows, and a slide-over that closes and
 * reopens keeps a sent (or sending) card from offering "Fix it" again.
 *
 * The server merges every cause for one subject into one row, so a key
 * can stay listed while the need behind it changes. Each entry therefore
 * records the row's fingerprint (`needFingerprint`) and only applies to a
 * row that still has it. Nothing is persisted.
 */
export interface TodoLocal {
  hidden: ReadonlyMap<string, string>;
  done: ReadonlyMap<string, { fp: string; text: string }>;
  sent: ReadonlyMap<string, string>;
  sending: ReadonlyMap<string, string>;
}

const empty = (): TodoLocal => ({ hidden: new Map(), done: new Map(), sent: new Map(), sending: new Map() });
let state: TodoLocal = empty();
const listeners = new Set<() => void>();

function set(next: TodoLocal) {
  state = next;
  for (const fn of listeners) fn();
}

const mark = (m: ReadonlyMap<string, string>, need: HomeNeed) => new Map(m).set(need.key, needFingerprint(need));
const unmark = (m: ReadonlyMap<string, string>, need: HomeNeed) => {
  const next = new Map(m);
  next.delete(need.key);
  return next;
};

export const hideTodo = (need: HomeNeed) => set({ ...state, hidden: mark(state.hidden, need) });
export const doneTodo = (need: HomeNeed, text: string) =>
  set({ ...state, done: new Map(state.done).set(need.key, { fp: needFingerprint(need), text }) });
export const sendingTodo = (need: HomeNeed) => set({ ...state, sending: mark(state.sending, need) });
/** A send ended: success marks the card sent, failure leaves it idle. */
export const settleTodo = (need: HomeNeed, ok: boolean) =>
  set({ ...state, sending: unmark(state.sending, need), sent: ok ? mark(state.sent, need) : state.sent });

const has = (m: ReadonlyMap<string, string>, need: HomeNeed) => m.get(need.key) === needFingerprint(need);

/** The marks that still apply to this row. */
export function marksFor(local: TodoLocal, need: HomeNeed) {
  const d = local.done.get(need.key);
  return {
    hidden: has(local.hidden, need),
    doneText: d && d.fp === needFingerprint(need) ? d.text : undefined,
    sent: has(local.sent, need),
    sending: has(local.sending, need),
  };
}

/** A card the operator already hid or decided: it left the list and the count. */
export const isSettled = (local: TodoLocal, need: HomeNeed) => {
  const m = marksFor(local, need);
  return m.hidden || m.doneText !== undefined;
};

/** Forget entries whose row is gone or has changed, so a returning need starts fresh. */
export function reconcileTodo(current: readonly HomeNeed[]) {
  const fp = new Map(current.map((n) => [n.key, needFingerprint(n)]));
  const keep = <V>(m: ReadonlyMap<string, V>, pick: (v: V) => string) =>
    new Map([...m].filter(([k, v]) => fp.get(k) === pick(v)));
  const same = (a: ReadonlyMap<string, unknown>, b: ReadonlyMap<string, unknown>) => a.size === b.size;
  const next: TodoLocal = {
    hidden: keep(state.hidden, (v) => v),
    done: keep(state.done, (v) => v.fp),
    sent: keep(state.sent, (v) => v),
    sending: keep(state.sending, (v) => v),
  };
  if (same(next.hidden, state.hidden) && same(next.done, state.done) && same(next.sent, state.sent) && same(next.sending, state.sending)) return;
  set(next);
}

/** Clears everything — for tests, which share one module. */
export const resetTodoLocal = () => set(empty());

export function useTodoLocal(): TodoLocal {
  return useSyncExternalStore(
    (fn) => {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    () => state,
  );
}

/** The sidebar's Home badge: pending To do items, less what the operator just settled. */
export function useHomeCount(rows: NeedsMe[] | null | undefined): number {
  const local = useTodoLocal();
  const needs = rows ? todoSplit(rows).todo : null;
  const sig = needs?.map((n) => `${n.key}\t${needFingerprint(n)}`).join("\n") ?? null;
  useEffect(() => {
    if (needs) reconcileTodo(needs);
  }, [sig]);
  return todoCount(rows, (n) => isSettled(local, n));
}
