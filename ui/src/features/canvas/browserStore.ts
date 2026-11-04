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
 * - poll `GET /api/browser` only while a session is live (`active`,
 *   `handoff`) or being stopped: a takeover ends in the live view's own Done
 *   control, and this is how the board notices;
 * - no viewer URL is ever held here (see browserClient.viewBrowser).
 */
import { useSyncExternalStore } from "react";
import { onArtifactFocus } from "./canvasStore";
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
  /** The tab's x was used: no tab, session still running. */
  hidden: boolean;
  /** The drawer was opened on the Browser tab (artifact tabs open it too). */
  open: boolean;
  /** The Browser tab is the active tab. */
  focused: boolean;
  stop: StopPhase;
  stopStartedAt: number | null;
  /** A takeover just ended: handoff -> active seen. Cleared on the next change. */
  handedBack: boolean;
}

const POLL = { active: 3000, handoff: 2000 };
const listeners = new Set<() => void>();
let state: BrowserTabState = empty(null);
/** Bumped on every conversation change and read, so a stale answer is dropped. */
let epoch = 0;
let timer: ReturnType<typeof setTimeout> | null = null;
let reading = false;

function empty(conversation: string | null): BrowserTabState {
  return {
    conversation,
    availability: "unknown",
    session: null,
    loadError: false,
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
  if (s.conversation === null || s.availability !== "available") return null;
  if (s.session?.state === "handoff") return POLL.handoff;
  if (s.session?.state === "active" || s.stop === "stopping") return POLL.active;
  return null;
}

/** Tests only: shorten the poll interval. */
export function setBrowserPollMs(active: number, handoff: number): void {
  POLL.active = active;
  POLL.handoff = handoff;
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

function apply(read: Awaited<ReturnType<typeof readBrowser>>): void {
  if (read.kind === "unavailable") {
    set({ ...state, availability: "unavailable", session: null, loadError: false, open: false, focused: false, stop: "idle" });
    return;
  }
  const prev = state.session?.state;
  const next = read.session;
  const handedBack = prev === "handoff" && next.state === "active";
  // A new takeover request brings a hidden tab back.
  const asked = next.state === "handoff" && prev !== "handoff";
  const over = next.state === "ended" || next.state === "none";
  set({
    ...state,
    availability: "available",
    session: next,
    loadError: false,
    hidden: asked ? false : state.hidden,
    stop: over ? "idle" : state.stop,
    stopStartedAt: over ? null : state.stopStartedAt,
    handedBack,
  });
}

/** Read the session now. One read at a time; a stale answer is dropped. */
export async function refreshBrowser(): Promise<void> {
  const conversation = state.conversation;
  if (conversation === null || reading) return;
  reading = true;
  const mine = epoch;
  try {
    const read = await readBrowser(conversation);
    if (mine === epoch) apply(read);
  } catch {
    // Keep what is shown. Only a tab that already exists says it cannot load;
    // a board we have never heard from offers none.
    if (mine === epoch && state.availability === "available" && !state.loadError) set({ ...state, loadError: true });
  } finally {
    if (mine === epoch) {
      reading = false;
      schedule();
    }
  }
}

/** Home's explicit wiring: the conversation in view. A change resets all. */
export function feedBrowser(conversation: string | null): void {
  if (conversation === state.conversation) return;
  epoch++;
  reading = false;
  if (timer !== null) clearTimeout(timer);
  timer = null;
  set(empty(conversation));
  if (conversation !== null) void refreshBrowser();
}

/** Test and sign-out reset. */
export function resetBrowser(): void {
  feedBrowser(null);
}

export function openBrowser(): void {
  if (state.availability !== "available") return;
  set({ ...state, hidden: false, open: true, focused: true });
}

export function focusBrowser(): void {
  if (tabVisible(state) && !state.focused) set({ ...state, focused: true });
}

/** The tab's x: hide it. The session keeps running. */
export function hideBrowser(): void {
  if (!state.hidden) set({ ...state, hidden: true, focused: false, open: false });
}

/** The drawer closed. Nothing about the session changes. */
export function closeBrowserDrawer(): void {
  if (state.open || state.focused) set({ ...state, open: false });
}

// An artifact taking focus (card click, tab click, auto-open) ends the browser's.
onArtifactFocus(() => {
  if (state.focused) set({ ...state, focused: false });
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
  const prior = state.session;
  set({
    ...state,
    stop: "idle",
    stopStartedAt: null,
    session: prior ? { ...prior, state: "ended", instructions: null } : prior,
  });
  void refreshBrowser();
}
