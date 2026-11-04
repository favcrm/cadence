/**
 * CAD-1726: the adapter every conversation host (Home, the app pane) uses
 * to hand its thread to the canvas. It reads artifact cards and the running
 * turn out of the `ThreadItem`s the host already renders — nothing is ever
 * parsed from message text, tool output or URLs.
 */
import { useEffect, useRef } from "react";
import { isArtifactPutCall, toolSteps, type ThreadItem } from "../home/thread";
import {
  ARTIFACT_KINDS,
  feedCanvas,
  type ArtifactKind,
  type ArtifactRef,
  type FeedCard,
  type Surface,
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

/** Artifact ids a tool call asks to add a version to: `artifact put … --id art-x`
 *  (the thread keeps only the first 160 characters of a command, so the
 *  master gives `--id` first). */
function putTargets(text: string): string[] {
  if (!isArtifactPutCall(text)) return [];
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
 * the message id of the turn the host says is running (null when none); the
 * strip and the chip follow that turn, not the newest operator message, which
 * may be a follow-up queued behind it.
 */
export function feedParts(items: ThreadItem[], liveAfter: number, running: string | null): FeedParts {
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
  if (running !== null) {
    turn = running;
    const own = (m: string | null | undefined) => !m || m === running;
    const start = items.findIndex((i) => i.type === "operator" && i.entry.message === running);
    const tail = items.slice((start >= 0 ? start : lastOperator) + 1);
    const updating = new Set<string>();
    for (const item of tail) {
      if (item.type !== "tools") continue;
      for (const s of toolSteps(item.entries.filter((x) => own(x.message)))) {
        // Only a put still waiting for its result is "updating"; a landed one
        // reads "updated to vN" from the tab itself.
        if (!s.done) for (const id of putTargets(s.call.text)) updating.add(id);
      }
    }
    working = { turn, updating: [...updating] };
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
  running: string | null;
  /** Default true. False keeps a card from opening the drawer by itself. */
  autoOpen?: boolean;
}): void {
  const { conversation, surface, composerKey, items, loaded, running, autoOpen } = opts;
  const mark = useRef<{ conversation: string | null; seq: number | null }>({ conversation: null, seq: null });
  useEffect(() => {
    if (mark.current.conversation !== conversation) mark.current = { conversation, seq: null };
    if (!loaded || conversation === null) return;
    if (mark.current.seq === null) mark.current.seq = topSeq(items);
    feedCanvas({ conversation, surface, composerKey, autoOpen, ...feedParts(items, mark.current.seq, running) });
  }, [conversation, surface, composerKey, items, loaded, running, autoOpen]);
}
