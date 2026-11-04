/**
 * CAD-2103: the Browser tab's state. Deliberately NOT in canvasStore.ts,
 * which models versioned artifacts: a browser session is live state the
 * board owns, read by polling, and it outlives the drawer.
 *
 * Rules (the ticket's):
 * - a board with no browser (`{state: "unavailable"}`, or a 404 from one that
 *   predates the routes) offers no tab at all; `none` and `ended` keep it;
 * - the tab's x hides it and the session keeps running; closing the drawer
 *   never stops anything; only Stop does;
 * - once this client has seen a session live, a later `unavailable` or 404 is
 *   a failed read, not an absent browser: the tab, Stop and polling stay;
 * - poll `GET /api/browser` only while a session is live (`active`,
 *   `handoff`) or being stopped: a takeover ends in the live view's own Done
 *   control, and this is how the board notices;
 * - no viewer URL is ever held here (see browserClient.viewBrowser).
 */
import { useSyncExternalStore } from "react";
import { canvasSnapshot, onArtifactFocus, onArtifactsEmptied, showDrawer } from "./canvasStore";
import { onPrivateSessionReset } from "../../lib/privateSession";
import { readBrowser, stopBrowser, type BrowserSession } from "./browserClient";

export type StopPhase = "idle" | "confirm" | "stopping" | "failed";

export interface BrowserTabState {
  conversation: string | null;
  /** `unknown` until the first good answer; only `available` shows a tab. */
  availability: "unknown" | "unavailable" | "available";
  /** The last good answer. */
  session: BrowserSession | null;
  /** The last read failed (the session, if any, is unaffected). */
  loadError: boolean;
  /** A session has been seen live (active or handoff) in this conversation. */
  everLive: boolean;
  /** The first reads all failed, so the board is not known either way yet. */
  probeFailed: boolean;
  /** The tab's x was used: no tab, session still running. */
  hidden: boolean;
  /** The drawer was opened on the Browser tab (artifact tabs open it too). */
  open: boolean;
  /** The Browser tab is the active tab. */
  focused: boolean;
  stop: StopPhase;
  stopStartedAt: number | null;
  /** A takeover just ended: handoff -> active seen. Cleared by the next read. */
  handedBack: boolean;
}

const POLL = { active: 3000, handoff: 2000, probe: 30_000 } as const;
const listeners = new Set<() => void>();
let state: BrowserTabState = empty(null);
/** Bumped on every conversation change and on a confirmed stop, so a stale answer is dropped. */
let epoch = 0;
let timer: ReturnType<typeof setTimeout> | null = null;
let reading = false;
/** A read was asked for while one was in flight: it runs once that one ends. */
let again = false;

function empty(conversation: string | null): BrowserTabState {
  return {
    conversation,
    availability: "unknown",
    session: null,
    loadError: false,
    everLive: false,
    probeFailed: false,
    hidden: false,
    open: false,
    focused: false,
    stop: "idle",
    stopStartedAt: null,
    handedBack: false,
  };
}

function set(next: BrowserTabState): void {
  if (next === state) return;
  state = next;
  schedule();
  for (const l of [...listeners]) l();
}

export function browserSnapshot(): BrowserTabState {
  return state;
}

export function useBrowserTab(): BrowserTabState {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    browserSnapshot,
    browserSnapshot,
  );
}

/** The tab exists: the board has a browser and the person has not hidden it. */
export function tabVisible(s: BrowserTabState): boolean {
  return s.availability === "available" && !s.hidden;
}

/** Poll only while a session is live or being stopped. */
export function pollDelay(s: BrowserTabState): number | null {
  if (s.conversation === null) return null;
  // A board that has not answered at all is tried again, slowly: a reload in
  // the middle of a takeover must not leave a live session with no Stop. A
  // definite unavailable or 404 ends it.
  if (s.availability === "unknown") return s.probeFailed ? POLL.probe : null;
  if (s.availability !== "available") return null;
  if (s.session?.state === "handoff") return POLL.handoff;
  if (s.session?.state === "active" || s.stop === "stopping") return POLL.active;
  return null;
}

function schedule(): void {
  const delay = pollDelay(state);
  if (delay === null) {
    if (timer !== null) clearTimeout(timer);
    timer = null;
    return;
  }
  if (timer !== null) return;
  timer = setTimeout(() => {
    timer = null;
    // A hidden tab does not poll; the next tick (or visibility) catches up.
    if (typeof document !== "undefined" && document.hidden) schedule();
    else void refreshBrowser();
  }, delay);
}

const sameSession = (a: BrowserSession | null, b: BrowserSession | null): boolean =>
  a === b || (a !== null && b !== null && a.state === b.state && a.signedIn === b.signedIn && a.url === b.url && a.instructions === b.instructions);

function apply(read: Awaited<ReturnType<typeof readBrowser>>): void {
  if (read.kind === "unavailable") {
    // A board that answered with a live session does not lose its browser: this
    // is a failed read, so the tab, Stop and polling stay.
    if (state.everLive) {
      if (!state.loadError) set({ ...state, loadError: true });
      return;
    }
    set({ ...state, availability: "unavailable", session: null, loadError: false, probeFailed: false, open: false, focused: false, stop: "idle" });
    return;
  }
  const prev = state.session?.state;
  const next = read.session;
  const handedBack = prev === "handoff" && next.state === "active";
  // A new takeover request brings a hidden tab back.
  const asked = next.state === "handoff" && prev !== "handoff";
  const over = next.state === "ended" || next.state === "none";
  const nextState: BrowserTabState = {
    ...state,
    availability: "available",
    // The same session keeps its identity, so an unchanged poll changes nothing.
    session: sameSession(state.session, next) ? state.session : next,
    loadError: false,
    probeFailed: false,
    everLive: state.everLive || next.state === "active" || next.state === "handoff",
    hidden: asked ? false : state.hidden,
    // A failed stop is about the phase it failed in.
    stop: over || (state.stop === "failed" && prev !== next.state) ? "idle" : state.stop,
    stopStartedAt: over ? null : state.stopStartedAt,
    handedBack,
  };
  // An unchanged answer is no change: a `set` would re-render every card.
  const same = (Object.keys(nextState) as (keyof BrowserTabState)[]).every((k) => nextState[k] === state[k]);
  if (!same) set(nextState);
}

/** Read the session now. One read at a time; an answer from before a
 *  conversation change or a confirmed stop is dropped (see `epoch`). */
export async function refreshBrowser(): Promise<void> {
  const conversation = state.conversation;
  if (conversation === null) return;
  if (reading) {
    again = true;
    return;
  }
  reading = true;
  const mine = epoch;
  try {
    const read = await readBrowser(conversation); // bounded: a hung read fails
    if (mine === epoch) apply(read);
  } catch {
    // Keep what is shown. Only a tab that already exists says it cannot load;
    // a board we have never heard from offers none.
    if (mine === epoch) {
      if (state.availability === "available") {
        if (!state.loadError) set({ ...state, loadError: true });
      } else if (state.availability === "unknown" && !state.probeFailed) {
        set({ ...state, probeFailed: true });
      }
    }
  } finally {
    if (mine === epoch) {
      reading = false;
      schedule();
      if (again) {
        again = false;
        void refreshBrowser();
      }
    }
  }
}

/** Home's explicit wiring: the conversation in view. A change resets all. */
export function feedBrowser(conversation: string | null): void {
  if (conversation === state.conversation) return;
  epoch++;
  reading = false;
  again = false;
  if (timer !== null) clearTimeout(timer);
  timer = null;
  set(empty(conversation));
  if (conversation !== null) void refreshBrowser();
}

/** Test and sign-out reset. */
export function resetBrowser(): void {
  feedBrowser(null);
}

onPrivateSessionReset(resetBrowser);

export function openBrowser(): void {
  if (state.availability !== "available") return;
  set({ ...state, hidden: false, open: true, focused: true });
}

export function focusBrowser(): void {
  if (tabVisible(state) && !state.focused) set({ ...state, focused: true });
}

/** The tab's x: hide it. The session keeps running. A drawer this tab alone
 *  opened hands its open state to the artifact tabs it holds, so hiding the
 *  tab never closes the drawer over an artifact. */
export function hideBrowser(): void {
  if (state.hidden) return;
  if (state.open && canvasSnapshot().tabs.length > 0) showDrawer();
  set({ ...state, hidden: true, focused: false, open: false, stop: state.stop === "confirm" ? "idle" : state.stop });
}

/** The drawer closed. Nothing about the session changes. */
export function closeBrowserDrawer(): void {
  // A confirm left pending must not wait there when the drawer reopens.
  if (state.open || state.focused || state.stop === "confirm") {
    set({ ...state, open: false, focused: false, stop: state.stop === "confirm" ? "idle" : state.stop });
  }
}

// An artifact taking focus (card click, tab click, auto-open) ends the browser's,
// except that the feed's auto-open never pulls the person off a takeover.
onArtifactFocus((source) => {
  if (source === "auto" && state.session?.state === "handoff") return;
  if (state.focused) set({ ...state, focused: false });
});

// Closing the last artifact keeps the drawer, and a live view in it, on the
// Browser tab only when it has something to show: a live session, or a drawer
// the person opened on it. A tab with no session would be an empty panel.
onArtifactsEmptied(() => {
  const live = state.session?.state === "active" || state.session?.state === "handoff";
  if (!tabVisible(state) || !(live || state.open)) return false;
  set({ ...state, open: true, focused: true });
  return true;
});

/** Stop asks first only when something would be lost: a signed-in session or
 *  a takeover. Otherwise it stops straight away. */
export function needsStopConfirm(s: BrowserTabState): boolean {
  return s.session?.signedIn === true || s.session?.state === "handoff";
}

export function requestStop(): void {
  if (state.stop === "stopping") return;
  if (needsStopConfirm(state)) set({ ...state, stop: "confirm" });
  else void confirmStop();
}

export function cancelStop(): void {
  if (state.stop === "confirm") set({ ...state, stop: "idle" });
}

/** Stopping can take minutes on a hung page; the request stays pending and
 *  the panel says so. Failure (503) means NOT stopped. */
export async function confirmStop(): Promise<void> {
  const conversation = state.conversation;
  if (conversation === null || state.stop === "stopping") return;
  const mine = epoch;
  set({ ...state, stop: "stopping", stopStartedAt: Date.now(), handedBack: false });
  try {
    await stopBrowser(conversation);
  } catch {
    if (mine === epoch && browserSnapshot().stop === "stopping") set({ ...state, stop: "failed" });
    return;
  }
  if (mine !== epoch) return;
  // A read already in flight may carry the session as it was before the stop:
  // drop it, so it cannot bring `active` and Stop back.
  epoch++;
  reading = false;
  again = false;
  const prior = state.session;
  set({
    ...state,
    stop: "idle",
    stopStartedAt: null,
    session: prior ? { ...prior, state: "ended", instructions: null } : prior,
  });
  void refreshBrowser();
}
