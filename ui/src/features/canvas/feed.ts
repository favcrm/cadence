/**
 * CAD-1726: the adapter every conversation host (Home, the app pane) uses
 * to hand its thread to the canvas. It reads artifact cards and the running
 * turn out of the `ThreadItem`s the host already renders — nothing is ever
 * parsed from message text, tool output or URLs.
 */
import { useEffect, useRef } from "react";
import { openStep, stepSummary, toolSteps, type ThreadItem } from "../home/thread";
import {
  ARTIFACT_KINDS,
  feedCanvas,
  type ArtifactKind,
  type ArtifactRef,
  type FeedCard,
  type Surface,
  type WorkStep,
  type Working,
} from "./canvasStore";

/** A stored artifact card's payload, validated; null when malformed. */
export function artifactRef(payload: Record<string, unknown> | null | undefined): ArtifactRef | null {
  if (!payload) return null;
  const { id, version, kind, title, bytes, digest } = payload;
  if (typeof id !== "string" || !/^art-[A-Za-z0-9]+$/.test(id)) return null;
  if (typeof version !== "number" || !Number.isInteger(version) || version < 1) return null;
  if (typeof kind !== "string" || !ARTIFACT_KINDS.includes(kind as ArtifactKind)) return null;
  return {
    id,
    version,
    kind: kind as ArtifactKind,
    title: typeof title === "string" && title.trim() ? title : "Untitled",
    bytes: typeof bytes === "number" && Number.isFinite(bytes) ? bytes : 0,
    digest: typeof digest === "string" && /^[0-9a-f]{64}$/.test(digest) ? digest : undefined,
  };
}

/** Plain words for a tool step: the tool's name decides, never its
 *  arguments — no command line, path or JSON reaches the Working view. The
 *  raw step stays in the "details" disclosure. */
export function plainStep(text: string): string {
  const raw = stepSummary(text);
  const tool = /^([A-Za-z_][\w .-]{0,30}?):/.exec(raw)?.[1]?.trim().toLowerCase() ?? "";
  if (/\bartifact put\b/.test(raw)) return "Putting it in the canvas";
  if (tool === "read" || tool === "cat" || tool === "grep" || tool === "glob" || tool === "ls") return "Reading";
  if (tool === "write" || tool === "edit") return "Writing a draft";
  if (tool === "bash") return "Checking the board";
  return "Working on it";
}

/** A word that names a filesystem path: it starts with `/`, `~/`, `./` or
 *  `../` (after any opening punctuation), has a `/` after `=` or `:`, or
 *  names the master's tmp or the state dir. */
const PATHISH = /^[("'`[{<]*(?:\/|~\/|\.\/|\.\.\/)|[=:][^\s]*\/|master\/tmp|\.local\/state/;

/** The agent's own words with every path-like word replaced by an ellipsis. */
function plainWords(text: string): string {
  return text
    .split(/(\s+)/)
    .map((w) => (PATHISH.test(w) ? "…" : w))
    .join("");
}

/** Artifact ids a tool call asks to add a version to: `artifact put … --id art-x`. */
function putTargets(text: string): string[] {
  if (!/\bartifact put\b/.test(text)) return [];
  const m = /--id[ =](art-[A-Za-z0-9]+)/.exec(text);
  return m ? [m[1]] : [];
}

export interface FeedParts {
  cards: FeedCard[];
  working: Working | null;
  turn: string | null;
}

/**
 * `liveAfter`: entries at or below this seq are history (the host's mount
 * point); only later ones may open the drawer by themselves. `running`:
 * the host's own word that a turn is in flight.
 */
export function feedParts(items: ThreadItem[], liveAfter: number, running: boolean): FeedParts {
  const operatorTurns = new Set<string>();
  let turn: string | null = null;
  let lastOperator = -1;
  items.forEach((item, i) => {
    if (item.type === "operator" && item.entry.message) {
      operatorTurns.add(item.entry.message);
      turn = item.entry.message;
      lastOperator = i;
    }
  });
  const cards: FeedCard[] = [];
  for (const item of items) {
    if (item.type !== "artifact") continue;
    const ref = artifactRef(item.entry.payload);
    if (!ref) continue;
    const message = item.entry.message ?? null;
    cards.push({
      key: `${ref.id}@${ref.version}`,
      ref,
      live: item.entry.seq > liveAfter,
      operatorTurn: message !== null && operatorTurns.has(message),
      turn: message,
    });
  }
  let working: Working | null = null;
  if (running) {
    const tail = items.slice(lastOperator + 1);
    let plan: string | null = null;
    const steps: WorkStep[] = [];
    const updating = new Set<string>();
    for (const item of tail) {
      if (item.type === "commentary" && item.entry.text.trim()) plan = plainWords(item.entry.text.trim());
      if (item.type === "tools") {
        for (const s of toolSteps(item.entries)) {
          steps.push({ text: plainStep(s.call.text), raw: s.summary, done: s.done });
          for (const id of putTargets(s.call.text)) updating.add(id);
        }
      }
    }
    const tools = [...tail].reverse().find((i) => i.type === "tools");
    const open = tools && tools.type === "tools" ? openStep(tools.entries) : null;
    working = {
      plan,
      step: open ? plainStep(open.text) : null,
      steps: steps.slice(-4),
      count: steps.length,
      turn,
      updating: [...updating],
    };
  }
  return { cards, working, turn };
}

/** The highest stored seq among the items — the mount watermark. */
export function topSeq(items: ThreadItem[]): number {
  let top = 0;
  for (const item of items) {
    if (item.type === "pending") continue;
    const entries = item.type === "tools" ? item.entries : [item.entry];
    for (const e of entries) top = Math.max(top, e.seq);
  }
  return top;
}

/**
 * Feed the canvas from a host's thread. The first loaded page of a
 * conversation is history (its artifacts never open the drawer); anything
 * stored after it is live.
 */
export function useCanvasFeed(opts: {
  conversation: string | null;
  surface: Surface;
  composerKey: string | null;
  items: ThreadItem[];
  loaded: boolean;
  running: boolean;
}): void {
  const { conversation, surface, composerKey, items, loaded, running } = opts;
  const mark = useRef<{ conversation: string | null; seq: number | null }>({ conversation: null, seq: null });
  useEffect(() => {
    if (mark.current.conversation !== conversation) mark.current = { conversation, seq: null };
    if (!loaded || conversation === null) return;
    if (mark.current.seq === null) mark.current.seq = topSeq(items);
    feedCanvas({ conversation, surface, composerKey, ...feedParts(items, mark.current.seq, running) });
  }, [conversation, surface, composerKey, items, loaded, running]);
}
