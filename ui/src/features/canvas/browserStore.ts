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
 *   control, and this is how the board notices. While the host says a turn is
 *   running it polls in any state, because the assistant starts a session or
 *   asks for a takeover partway through a turn, with no thread entry to say
 *   so, and reads once more when the turn settles. A running turn is
 *   watched for at most 3 minutes after its thread last changed, so polling is
 *   bounded; an idle `none` never polls, and a board that answered
 *   unavailable is never read again;
 * - no viewer URL is ever held here (see browserClient.viewBrowser).
 */
import { useEffect, useState, useSyncExternalStore } from "react";
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
}

const POLL = { active: 3000, handoff: 2000, turn: 2000, probe: 30_000 } as const;
/** Failed reads in a row before the person is told, once. */
const ALERT_AFTER = 3;
const listeners = new Set<() => void>();
let state: BrowserTabState = empty(null);
/** Bumped on every conversation change and on a confirmed stop, so a stale answer is dropped. */
let epoch = 0;
let timer: ReturnType<typeof setTimeout> | null = null;
let reading = false;
/** A read was asked for while one was in flight: it runs once that one ends. */
let again = false;
/** The host's turn is running (see `useBrowserFeed`). */
let watching = false;
let failedReads = 0;

const alerts = new Set<(text: string) => void>();
/** Words for the board's toast: a failure the person has to act on or would
 *  otherwise be misled by. English; the listener translates. */
export function onBrowserAlert(listener: (text: string) => void): () => void {
  alerts.add(listener);
  return () => alerts.delete(listener);
}
export function alertBrowser(text: string): void {
  for (const l of [...alerts]) l(text);
}

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

/** Poll while a session is live or being stopped, or a turn is running. */
export function pollDelay(s: BrowserTabState, turnRunning = false): number | null {
  if (s.conversation === null) return null;
  // A board that has not answered at all is tried again, slowly: a reload in
  // the middle of a takeover must not leave a live session with no Stop. A
  // definite unavailable or 404 ends it.
  if (s.availability === "unknown") return s.probeFailed ? POLL.probe : null;
  if (s.availability !== "available") return null;
  if (s.session?.state === "handoff") return POLL.handoff;
  if (s.session?.state === "active" || s.stop === "stopping") return POLL.active;
  return turnRunning ? POLL.turn : null;
}

function schedule(): void {
  const delay = pollDelay(state, watching);
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
      readFailed();
      return;
    }
    set({ ...state, availability: "unavailable", session: null, loadError: false, probeFailed: false, open: false, focused: false, stop: "idle" });
    return;
  }
  failedReads = 0;
  const prev = state.session?.state;
  const next = read.session;
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
  };
  // An unchanged answer is no change: a `set` would re-render every card.
  const same = (Object.keys(nextState) as (keyof BrowserTabState)[]).every((k) => nextState[k] === state[k]);
  if (!same) set(nextState);
}

/** A failed read keeps what is shown. One failure is a blip; only a run of them,
 *  and once, says the state cannot be loaded; only for a live session. */
function readFailed(): void {
  // With no live session there is nothing to lose sight of: no session, no noise.
  if (state.session?.state !== "active" && state.session?.state !== "handoff" && state.stop === "idle") return;
  if (!state.loadError) set({ ...state, loadError: true });
  if (++failedReads === ALERT_AFTER) alertBrowser("Can't load the browser state.");
}

/** Read the session now. One read at a time; an answer from before a
 *  conversation change or a confirmed stop is dropped (see `epoch`). */
export async function refreshBrowser(): Promise<void> {
  const conversation = state.conversation;
  // A board that answered unavailable has no browser: it is not asked again.
  if (conversation === null || state.availability === "unavailable") return;
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
      if (state.availability === "available") readFailed();
      else if (state.availability === "unknown" && !state.probeFailed) {
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
  failedReads = 0;
  if (conversation === null) watching = false;
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

/** A running turn is watched for this long after the thread last changed (or the
 *  turn started): one bound for every host, however the host decides it runs. */
const TURN_WINDOW_MS = 3 * 60_000;

/** The host's turn started or settled. While it runs the store polls in any
 *  state; the turn settling reads once more, for a session started after the
 *  last entry was read. */
export function watchBrowser(running: boolean): void {
  if (running === watching) return;
  watching = running;
  schedule();
  if (!running) void refreshBrowser();
}

/** A host's whole wiring: the conversation in view, whether its turn is
 *  running, and a revision that changes when its thread does, a pending send
 *  included (a new entry is when a session may have started). A running turn is
 *  watched for TURN_WINDOW_MS from the last revision or the turn starting, then
 *  read once more and left; a new revision restarts it. Leaving ends it all. */
export function useBrowserFeed(conversation: string | null, running: boolean, revision: string): void {
  const [expired, setExpired] = useState(false);
  useEffect(() => {
    setExpired(false);
    if (!running) return;
    const id = setTimeout(() => setExpired(true), TURN_WINDOW_MS);
    return () => clearTimeout(id);
  }, [conversation, running, revision]);
  useEffect(() => {
    feedBrowser(conversation);
    return () => feedBrowser(null);
  }, [conversation]);
  useEffect(() => {
    if (conversation !== null) void refreshBrowser();
  }, [conversation, revision]);
  useEffect(() => {
    watchBrowser(running && !expired);
  }, [conversation, running, expired]);
}

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
  set({ ...state, stop: "stopping" });
  try {
    await stopBrowser(conversation);
  } catch {
    if (mine === epoch && browserSnapshot().stop === "stopping") {
      set({ ...state, stop: "failed" });
      alertBrowser("Couldn't stop the session. It may still be running.");
    }
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
    session: prior ? { ...prior, state: "ended", instructions: null } : prior,
  });
  void refreshBrowser();
}
