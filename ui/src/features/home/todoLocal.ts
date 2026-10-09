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
 *
 * A row without a `since` cannot tell one occurrence from the next (the
 * overview is not fetched while the operator is off Home), so its decided
 * and sent marks also belong to one visit: a new mount of the rail drops
 * them. A send in flight is not scoped, so a remount mid-send cannot send twice.
 */
export interface TodoLocal {
  hidden: ReadonlyMap<string, string>;
  done: ReadonlyMap<string, { fp: string; text: string }>;
  sent: ReadonlyMap<string, string>;
  sending: ReadonlyMap<string, string>;
  /** Counts mounts of the Home rail; see `scope`. */
  visit: number;
}

const empty = (visit = 0): TodoLocal => ({ hidden: new Map(), done: new Map(), sent: new Map(), sending: new Map(), visit });
let state: TodoLocal = empty();
const listeners = new Set<() => void>();

function set(next: TodoLocal) {
  state = next;
  for (const fn of listeners) fn();
}

/** The fingerprint a decided or sent mark records: plus the visit when the row has no `since`. */
const scope = (need: HomeNeed, visit: number) => (need.since === null ? `${needFingerprint(need)}|visit ${visit}` : needFingerprint(need));

const mark = (m: ReadonlyMap<string, string>, need: HomeNeed, fp = needFingerprint(need)) => new Map(m).set(need.key, fp);
const unmark = (m: ReadonlyMap<string, string>, need: HomeNeed) => {
  const next = new Map(m);
  next.delete(need.key);
  return next;
};

export const hideTodo = (need: HomeNeed) => set({ ...state, hidden: mark(state.hidden, need) });
export const doneTodo = (need: HomeNeed, text: string) =>
  set({ ...state, done: new Map(state.done).set(need.key, { fp: scope(need, state.visit), text }) });
export const sendingTodo = (need: HomeNeed) => set({ ...state, sending: mark(state.sending, need) });
/** A send ended: success marks the card sent, failure leaves it idle. */
export const settleTodo = (need: HomeNeed, ok: boolean) =>
  set({ ...state, sending: unmark(state.sending, need), sent: ok ? mark(state.sent, need, scope(need, state.visit)) : state.sent });

const has = (m: ReadonlyMap<string, string>, need: HomeNeed, fp = needFingerprint(need)) => m.get(need.key) === fp;

/** The Home rail mounted: marks of an earlier visit no longer apply to rows without a `since`. */
export const beginTodoVisit = () => set({ ...state, visit: state.visit + 1 });

/** The marks that still apply to this row. */
export function marksFor(local: TodoLocal, need: HomeNeed) {
  const d = local.done.get(need.key);
  return {
    hidden: has(local.hidden, need),
    doneText: d && d.fp === scope(need, local.visit) ? d.text : undefined,
    sent: has(local.sent, need, scope(need, local.visit)),
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
  const by = new Map(current.map((n) => [n.key, n]));
  const keep = <V>(m: ReadonlyMap<string, V>, pick: (v: V) => string, fpOf: (n: HomeNeed) => string) =>
    new Map([...m].filter(([k, v]) => { const n = by.get(k); return !!n && fpOf(n) === pick(v); }));
  const here = (n: HomeNeed) => scope(n, state.visit);
  const next: TodoLocal = {
    hidden: keep(state.hidden, (v) => v, needFingerprint),
    done: keep(state.done, (v) => v.fp, here),
    sent: keep(state.sent, (v) => v, here),
    sending: keep(state.sending, (v) => v, needFingerprint),
    visit: state.visit,
  };
  const same = (a: ReadonlyMap<string, unknown>, b: ReadonlyMap<string, unknown>) => a.size === b.size;
  if (same(next.hidden, state.hidden) && same(next.done, state.done) && same(next.sent, state.sent) && same(next.sending, state.sending)) return;
  set(next);
}

/** Clears everything — for tests, which share one module. */
export const resetTodoLocal = () => set(empty(state.visit));

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
