/**
 * CAD-1726: the agent canvas's state. One store for the whole shell: the
 * drawer is mounted once and Home and every app pane feed it through
 * `feedCanvas` (see feed.ts). No React here; `useCanvas` subscribes.
 *
 * Rules (the ticket's):
 * - the drawer opens by itself only for the FIRST artifact of a turn the
 *   operator started, and only when that artifact arrives live, and only on a
 *   host that has not turned auto-open off (`FeedInput.autoOpen`: the member
 *   chat does, so the drawer never covers the chat on a phone);
 * - a drawer the operator closed stays closed for the rest of that turn;
 * - the drawer closes when the conversation changes;
 * - opening an artifact already open focuses its tab; closing the active
 *   tab selects a neighbour; closing the last collapses the drawer, unless the
 *   Browser tab (browserStore.ts, not modelled here) has a live session or was
 *   opened on its own, and then the drawer stays on it.
 */
import { useSyncExternalStore } from "react";
import { onPrivateSessionReset } from "../../lib/privateSession";

export type ArtifactKind = "md" | "pdf" | "html" | "image" | "csv";
export const ARTIFACT_KINDS: readonly ArtifactKind[] = ["md", "pdf", "html", "image", "csv"];

/** What a thread card carries — never the content. */
export interface ArtifactRef {
  id: string;
  version: number;
  kind: ArtifactKind;
  title: string;
  bytes: number;
  digest?: string;
  /** A generated image's shape (`16:9`), shown on its overlay; absent otherwise. */
  aspect?: string;
}

/** What the board knows of one stored version (from its thread card). */
export interface VersionInfo {
  bytes: number;
  digest?: string;
}

export interface CanvasTab {
  id: string;
  kind: ArtifactKind;
  title: string;
  bytes: number;
  aspect?: string;
  /** The newest version the thread has announced. */
  latest: number;
  /** The version in view. */
  selected: number;
  /** Selected tracks `latest` until the operator picks an older one. */
  follow: boolean;
  /** Sizes and digests of the versions the thread has announced. */
  versions: Record<number, VersionInfo>;
  /** The turn that put the newest version, if known. */
  turn: string | null;
}

/** The turn running in the thread, and what it is changing in the canvas. */
export interface Working {
  turn: string | null;
  /** Artifact ids this turn has asked to add a version to (`--id`). */
  updating: string[];
}

export type Surface = "home" | "app";

export interface CanvasState {
  conversation: string | null;
  /** Which host feeds the canvas (decides how much room the chat keeps). */
  surface: Surface;
  /** The composer draft key of the conversation in view (Ask to change). */
  composerKey: string | null;
  open: boolean;
  tabs: CanvasTab[];
  active: string | null;
  working: Working | null;
}

/** One card in the conversation, as the feed describes it. */
export interface FeedCard {
  /** Stable per entry: `${id}@${version}`. */
  key: string;
  ref: ArtifactRef;
  /** Arrived after the view mounted (not history). */
  live: boolean;
  /** The turn it belongs to started with an operator message. */
  operatorTurn: boolean;
  /** The turn (queued message id) the card belongs to. */
  turn: string | null;
}

export interface FeedInput {
  conversation: string | null;
  surface: Surface;
  composerKey: string | null;
  cards: FeedCard[];
  working: Working | null;
  /** The turn in flight, if any — a close is remembered against it. */
  turn: string | null;
  /** Default true. False: no card ever opens the drawer by itself. */
  autoOpen?: boolean;
}

const listeners = new Set<() => void>();
let state: CanvasState = emptyState();
/** Per conversation bookkeeping; reset when the conversation changes. */
let seen = new Set<string>();
let closedTurns = new Set<string>();
let autoOpened = new Set<string>();
let currentTurn: string | null = null;

function emptyState(
  conversation: string | null = null,
  composerKey: string | null = null,
  surface: Surface = "home",
): CanvasState {
  return { conversation, surface, composerKey, open: false, tabs: [], active: null, working: null };
}

/** An artifact took the active tab: "user" for a card or tab click, "auto"
 *  for the feed's one auto-open per turn. The session tab (browserStore.ts) is
 *  not modelled here; it listens instead. */
export type FocusSource = "user" | "auto";
const artifactFocusListeners = new Set<(source: FocusSource) => void>();
export function onArtifactFocus(listener: (source: FocusSource) => void): void {
  artifactFocusListeners.add(listener);
}
function artifactFocused(source: FocusSource): void {
  for (const l of [...artifactFocusListeners]) l(source);
}
/** The last artifact tab closed. A listener answers true to keep the drawer
 *  open on something else it shows (the Browser tab). */
const emptiedListeners = new Set<() => boolean>();
export function onArtifactsEmptied(listener: () => boolean): void {
  emptiedListeners.add(listener);
}

function set(next: CanvasState): void {
  if (next === state) return;
  state = next;
  for (const l of [...listeners]) l();
}

export function canvasSnapshot(): CanvasState {
  return state;
}

export function subscribeCanvas(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useCanvas(): CanvasState {
  return useSyncExternalStore(subscribeCanvas, canvasSnapshot, canvasSnapshot);
}

/** Test and conversation-switch reset. */
export function resetCanvas(
  conversation: string | null = null,
  composerKey: string | null = null,
  surface: Surface = "home",
): void {
  seen = new Set();
  closedTurns = new Set();
  autoOpened = new Set();
  currentTurn = null;
  set(emptyState(conversation, composerKey, surface));
}

function sameWorking(a: Working | null, b: Working | null): boolean {
  return a === b || (a !== null && b !== null && JSON.stringify(a) === JSON.stringify(b));
}

function newTab(ref: ArtifactRef, turn: string | null): CanvasTab {
  return {
    id: ref.id,
    kind: ref.kind,
    title: ref.title,
    bytes: ref.bytes,
    ...(ref.aspect ? { aspect: ref.aspect } : {}),
    latest: ref.version,
    selected: ref.version,
    follow: true,
    versions: { [ref.version]: { bytes: ref.bytes, digest: ref.digest } },
    turn,
  };
}

function withVersion(t: CanvasTab, ref: ArtifactRef, turn: string | null): CanvasTab {
  const newer = ref.version > t.latest;
  return {
    ...t,
    versions: { ...t.versions, [ref.version]: { bytes: ref.bytes, digest: ref.digest } },
    ...(ref.aspect && !t.aspect ? { aspect: ref.aspect } : {}),
    ...(newer
      ? { title: ref.title, bytes: ref.bytes, latest: ref.version, selected: t.follow ? ref.version : t.selected, turn }
      : {}),
  };
}

/**
 * Fold the conversation's cards and working state in. Idempotent: the
 * feed calls it on every render of the thread, so only a card never seen
 * before can add a tab, move a version or open the drawer.
 */
export function feedCanvas(input: FeedInput): void {
  if (input.conversation !== state.conversation) resetCanvas(input.conversation, input.composerKey, input.surface);
  currentTurn = input.turn;
  let tabs = state.tabs;
  let open = state.open;
  let active = state.active;
  let focused = false;
  for (const card of input.cards) {
    if (seen.has(card.key)) continue;
    seen.add(card.key);
    const { ref } = card;
    // A card only ever updates a tab that is already open. A tab is added by
    // opening a card, or by the one auto-open per operator turn (CAD-651).
    if (tabs.some((t) => t.id === ref.id)) {
      tabs = tabs.map((t) => (t.id === ref.id ? withVersion(t, ref, card.turn) : t));
    }
    if (input.autoOpen !== false && card.live && card.operatorTurn && card.turn && !autoOpened.has(card.turn) && !closedTurns.has(card.turn)) {
      autoOpened.add(card.turn);
      if (!tabs.some((t) => t.id === ref.id)) tabs = [...tabs, newTab(ref, card.turn)];
      open = true;
      active = ref.id;
      focused = true;
    }
  }
  if (tabs.length === 0) {
    open = false;
    active = null;
  } else if (active === null || !tabs.some((t) => t.id === active)) {
    active = tabs[0].id;
  }
  if (focused) artifactFocused("auto");
  const composerKey = input.composerKey;
  const working = sameWorking(input.working, state.working) ? state.working : input.working;
  if (
    tabs === state.tabs && open === state.open && active === state.active &&
    composerKey === state.composerKey && working === state.working && input.surface === state.surface
  ) return;
  set({ ...state, tabs, open, active, composerKey, working, surface: input.surface });
}

/** Open (or focus) an artifact from its card. */
export function openArtifact(ref: ArtifactRef): void {
  const existing = state.tabs.find((t) => t.id === ref.id);
  const tabs = existing
    ? state.tabs.map((t) =>
        t.id !== ref.id
          ? t
          : {
              ...withVersion(t, ref, t.turn),
              selected: ref.version,
              follow: ref.version >= Math.max(t.latest, ref.version),
            },
      )
    : [...state.tabs, newTab(ref, null)];
  set({ ...state, tabs, active: ref.id, open: true });
  artifactFocused("user");
}

export function focusTab(id: string): void {
  if (!state.tabs.some((t) => t.id === id)) return;
  if (state.active !== id) set({ ...state, active: id });
  artifactFocused("user");
}

export function closeTab(id: string): void {
  const index = state.tabs.findIndex((t) => t.id === id);
  if (index < 0) return;
  const tabs = state.tabs.filter((t) => t.id !== id);
  if (tabs.length === 0) {
    const kept = [...emptiedListeners].some((l) => l());
    if (!kept) closeDrawerFor(currentTurn);
    set({ ...state, tabs, active: null, open: false });
    return;
  }
  const active = state.active === id ? tabs[Math.min(index, tabs.length - 1)].id : state.active;
  set({ ...state, tabs, active });
}

function closeDrawerFor(turn: string | null): void {
  if (turn) closedTurns.add(turn);
}

/** The operator closed the drawer: it stays closed for this turn. */
export function closeDrawer(): void {
  if (!state.open) return;
  closeDrawerFor(currentTurn);
  set({ ...state, open: false });
}

/** Show the drawer again over the tabs it already holds (the Browser tab
 *  was hiding itself out of a drawer that it alone had opened). */
export function showDrawer(): void {
  if (state.tabs.length > 0 && !state.open) set({ ...state, open: true });
}

// A confirmed identity change drops this conversation's tabs: titles and
// versions belong to the person who was signed in.
onPrivateSessionReset(() => resetCanvas());

export function selectVersion(id: string, version: number): void {
  set({
    ...state,
    tabs: state.tabs.map((t) => (t.id !== id ? t : { ...t, selected: version, follow: version >= t.latest })),
  });
}

// ---- width (remembered per browser; every storage access is guarded) ----

const WIDTH_KEY = "cadence.canvas.width";
export const MIN_WIDTH = 360;

/** `floor` lifts the minimum (a kind's default floor), and with it the cap
 *  when 60% of a narrow viewport would sit under that floor. */
export function clampWidth(px: number, viewport: number, floor = MIN_WIDTH): number {
  const max = Math.max(floor, Math.floor(viewport * 0.6));
  return Math.min(max, Math.max(floor, Math.round(px)));
}

/** The width when the operator has not dragged one: it scales with the
 *  viewport, wider for kinds that are read as a page. */
export function defaultWidth(kind: ArtifactKind, viewport: number): number {
  const [share, lo, hi] = defaultRange(kind);
  return Math.min(hi, Math.max(lo, Math.round(viewport * share)));
}

function defaultRange(kind: ArtifactKind): [number, number, number] {
  return kind === "html" || kind === "csv" || kind === "pdf" ? [0.55, 560, 960] : [0.45, 480, 760];
}

/** The default width, held to its kind's floor even where 60% of the
 *  viewport is smaller (a 900px window keeps a document at 560px). */
export function defaultWidthFor(kind: ArtifactKind, viewport: number): number {
  return clampWidth(defaultWidth(kind, viewport), viewport, defaultRange(kind)[1]);
}

/** The width the operator dragged to, or null (it wins over the default). */
export function readWidth(): number | null {
  try {
    const raw = globalThis.localStorage?.getItem(WIDTH_KEY);
    if (raw === null || raw === undefined) return null;
    const n = Number(raw);
    return Number.isFinite(n) && n >= MIN_WIDTH ? n : null;
  } catch {
    return null;
  }
}

export function writeWidth(px: number): void {
  try {
    globalThis.localStorage?.setItem(WIDTH_KEY, String(px));
  } catch {
    // Blocked storage: the width just is not remembered.
  }
}

/** push | cover | sheet — by viewport and the room the chat keeps. */
export type CanvasMode = "push" | "cover" | "sheet";
export const SHEET_BELOW = 640;
export const CHAT_MIN = 320;
/** The least an expand must add to be worth it (else it covers the chat). */
const EXPAND_GAIN = 96;
/** What the chat keeps when the canvas is expanded: a narrow strip. The chat
 *  panel is inset 32px, so 96px leaves about 64px of it showing. */
export const CHAT_STRIP = 96;
const SIDEBAR = 208;
/** The app chat pane's right edge on a tablet with the canvas open: 1rem page
 *  padding + the 22rem column `app-shell.css` gives it. */
export const APP_TABLET_CHAT_RIGHT = 16 + 352;

export interface DrawerLayout {
  mode: CanvasMode;
  width: number;
}

/**
 * The mockup's per-surface rule: push while the chat keeps its room (Home:
 * the chat's 320px; the app shell: the chat plus 20rem of app area). When it
 * does not fit, cover stops short of the chat — on Home it keeps the chat's
 * first 320px, in the app shell it ends at the chat pane's right edge
 * (`chatRight`, the pane's right edge with no drawer padding). If that leaves
 * less than the minimum width, the drawer is a full-screen sheet instead.
 */
export function drawerLayout(
  viewport: number,
  want: number,
  surface: Surface,
  chatRight: number | null,
  expanded = false,
): DrawerLayout {
  if (viewport < SHEET_BELOW) return { mode: "sheet", width: viewport };
  const main = viewport - (viewport >= 1024 ? SIDEBAR : 0);
  if (expanded) {
    // Expand: cover, taking most of the page, leaving a narrow strip of chat.
    // The app shell stops at the chat pane's right edge while that is wider
    // than the drawer already is; when it is not, expand would do nothing,
    // so it covers the chat down to the strip as Home does.
    const edge = surface === "app" && viewport < 1024 ? APP_TABLET_CHAT_RIGHT : chatRight;
    const edgeRoom = surface === "app" && edge !== null ? viewport - edge - 12 : main - CHAT_STRIP;
    const room = edgeRoom >= drawerLayout(viewport, want, surface, chatRight).width + EXPAND_GAIN ? edgeRoom : main - CHAT_STRIP;
    return room < MIN_WIDTH ? { mode: "sheet", width: viewport } : { mode: "cover", width: room };
  }
  const need = surface === "home" ? CHAT_MIN : CHAT_MIN + 320 + 42;
  if (main - want >= need) return { mode: "push", width: want };
  // Without a chat pane in the page (the operator navigated away from an app
  // conversation) there is no edge to stop at: use the page-based cap.
  // Tablet app shell: the board narrows the chat pane while the canvas is open
  // (app-shell.css, `[data-canvas]`), so its edge is known without measuring.
  const edge = surface === "app" && viewport < 1024 ? APP_TABLET_CHAT_RIGHT : chatRight;
  const cap = surface === "app" && edge !== null ? viewport - edge - 12 : main - CHAT_MIN - 28;
  if (cap < MIN_WIDTH) return { mode: "sheet", width: viewport };
  return { mode: "cover", width: Math.min(want, cap) };
}
