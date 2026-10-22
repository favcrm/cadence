/**
 * CAD-1726: the agent canvas's state. One store for the whole shell: the
 * drawer is mounted once and Home and every app pane feed it through
 * `feedCanvas` (see feed.ts). No React here; `useCanvas` subscribes.
 *
 * Rules (the ticket's):
 * - the drawer opens by itself only for the FIRST artifact of a turn the
 *   operator started, and only when that artifact arrives live;
 * - a drawer the operator closed stays closed for the rest of that turn;
 * - the drawer closes when the conversation changes;
 * - opening an artifact already open focuses its tab; closing the active
 *   tab selects a neighbour; closing the last collapses the drawer.
 */
import { useSyncExternalStore } from "react";

export type ArtifactKind = "md" | "pdf" | "html" | "image" | "csv";
export const ARTIFACT_KINDS: readonly ArtifactKind[] = ["md", "pdf", "html", "image", "csv"];

/** What a thread card carries — never the content. */
export interface ArtifactRef {
  id: string;
  version: number;
  kind: ArtifactKind;
  title: string;
  bytes: number;
}

export interface CanvasTab {
  id: string;
  kind: ArtifactKind;
  title: string;
  bytes: number;
  /** The newest version the thread has announced. */
  latest: number;
  /** The version in view. */
  selected: number;
  /** Selected tracks `latest` until the operator picks an older one. */
  follow: boolean;
}

/** The running turn in plain words: the agent's latest commentary and the live step. */
export interface Working {
  plan: string | null;
  step: string | null;
}

export interface CanvasState {
  conversation: string | null;
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
  composerKey: string | null;
  cards: FeedCard[];
  working: Working | null;
  /** The turn in flight, if any — a close is remembered against it. */
  turn: string | null;
}

const listeners = new Set<() => void>();
let state: CanvasState = emptyState();
/** Per conversation bookkeeping; reset when the conversation changes. */
let seen = new Set<string>();
let closedTurns = new Set<string>();
let autoOpened = new Set<string>();
let currentTurn: string | null = null;

function emptyState(conversation: string | null = null, composerKey: string | null = null): CanvasState {
  return { conversation, composerKey, open: false, tabs: [], active: null, working: null };
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
export function resetCanvas(conversation: string | null = null, composerKey: string | null = null): void {
  seen = new Set();
  closedTurns = new Set();
  autoOpened = new Set();
  currentTurn = null;
  set(emptyState(conversation, composerKey));
}

function sameWorking(a: Working | null, b: Working | null): boolean {
  return a === b || (a !== null && b !== null && a.plan === b.plan && a.step === b.step);
}

/**
 * Fold the conversation's cards and working state in. Idempotent: the
 * feed calls it on every render of the thread, so only a card never seen
 * before can add a tab, move a version or open the drawer.
 */
export function feedCanvas(input: FeedInput): void {
  if (input.conversation !== state.conversation) resetCanvas(input.conversation, input.composerKey);
  currentTurn = input.turn;
  let tabs = state.tabs;
  let open = state.open;
  let active = state.active;
  for (const card of input.cards) {
    if (seen.has(card.key)) continue;
    seen.add(card.key);
    const { ref } = card;
    const existing = tabs.find((t) => t.id === ref.id);
    if (!existing) {
      tabs = [...tabs, { id: ref.id, kind: ref.kind, title: ref.title, bytes: ref.bytes, latest: ref.version, selected: ref.version, follow: true }];
    } else if (ref.version > existing.latest) {
      tabs = tabs.map((t) =>
        t.id !== ref.id ? t : { ...t, title: ref.title, bytes: ref.bytes, latest: ref.version, selected: t.follow ? ref.version : t.selected },
      );
    }
    if (card.live && card.operatorTurn && card.turn && !autoOpened.has(card.turn) && !closedTurns.has(card.turn)) {
      autoOpened.add(card.turn);
      open = true;
      active = ref.id;
    }
  }
  if (!open && tabs.length === 0) active = null;
  if (active === null && tabs.length > 0) active = tabs[0].id;
  const composerKey = input.composerKey;
  if (
    tabs === state.tabs && open === state.open && active === state.active &&
    composerKey === state.composerKey && sameWorking(input.working, state.working)
  ) return;
  set({ ...state, tabs, open, active, composerKey, working: sameWorking(input.working, state.working) ? state.working : input.working });
}

/** Open (or focus) an artifact from its card. */
export function openArtifact(ref: ArtifactRef): void {
  const existing = state.tabs.find((t) => t.id === ref.id);
  const tabs = existing
    ? state.tabs.map((t) =>
        t.id !== ref.id ? t : { ...t, selected: ref.version, follow: ref.version >= t.latest, latest: Math.max(t.latest, ref.version) },
      )
    : [...state.tabs, { id: ref.id, kind: ref.kind, title: ref.title, bytes: ref.bytes, latest: ref.version, selected: ref.version, follow: true }];
  set({ ...state, tabs, active: ref.id, open: true });
}

export function focusTab(id: string): void {
  if (state.tabs.some((t) => t.id === id) && state.active !== id) set({ ...state, active: id });
}

export function closeTab(id: string): void {
  const index = state.tabs.findIndex((t) => t.id === id);
  if (index < 0) return;
  const tabs = state.tabs.filter((t) => t.id !== id);
  if (tabs.length === 0) {
    closeDrawerFor(currentTurn);
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

export function selectVersion(id: string, version: number): void {
  set({
    ...state,
    tabs: state.tabs.map((t) => (t.id !== id ? t : { ...t, selected: version, follow: version >= t.latest })),
  });
}

// ---- width (remembered per browser; every storage access is guarded) ----

const WIDTH_KEY = "cadence.canvas.width";
export const MIN_WIDTH = 360;
export const DEFAULT_WIDTH = 480;

export function clampWidth(px: number, viewport: number): number {
  const max = Math.max(MIN_WIDTH, Math.floor(viewport * 0.6));
  return Math.min(max, Math.max(MIN_WIDTH, Math.round(px)));
}

export function readWidth(): number {
  try {
    const n = Number(globalThis.localStorage?.getItem(WIDTH_KEY));
    return Number.isFinite(n) && n >= MIN_WIDTH ? n : DEFAULT_WIDTH;
  } catch {
    return DEFAULT_WIDTH;
  }
}

export function writeWidth(px: number): void {
  try {
    globalThis.localStorage?.setItem(WIDTH_KEY, String(px));
  } catch {
    // Blocked storage: the width just is not remembered.
  }
}

/** push | cover | sheet — by viewport and the page's minimum. */
export type CanvasMode = "push" | "cover" | "sheet";
export const SHEET_BELOW = 640;
export const PAGE_MIN = 560;
const SIDEBAR = 208;

export function canvasMode(viewport: number, width: number): CanvasMode {
  if (viewport < SHEET_BELOW) return "sheet";
  const side = viewport >= 1024 ? SIDEBAR : 0;
  return viewport - side - width >= PAGE_MIN ? "push" : "cover";
}
