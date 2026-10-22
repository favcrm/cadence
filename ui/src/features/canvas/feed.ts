/**
 * CAD-1726: the adapter every conversation host (Home, the app pane) uses
 * to hand its thread to the canvas. It reads artifact cards and the running
 * turn out of the `ThreadItem`s the host already renders — nothing is ever
 * parsed from message text, tool output or URLs.
 */
import { useEffect, useRef } from "react";
import { isArtifactPutCall, openStep, stepSummary, toolSteps, type ThreadItem } from "../home/thread";
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
  if (isArtifactPutCall(text)) return "Putting it in the canvas";
  if (tool === "read" || tool === "cat" || tool === "grep" || tool === "glob" || tool === "ls") return "Reading";
  if (tool === "write" || tool === "edit") return "Writing a draft";
  if (tool === "bash") return "Checking the board";
  return "Working on it";
}

/** A word that names a filesystem path: it starts with `/`, `~/`, `./` or
 *  `../` (after any opening punctuation), has a `/` after `=` or `:`, or
 *  names the master's tmp or the state dir. */
const PATHISH = /^[("'`[{<]*(?:\/|~\/|\.\/|\.\.\/)|[=:][^\s=:]*\/|master\/tmp|\.local\/state/;

/** The longest JSON span `scrubJson` will look at. */
const JSON_SPAN_MAX = 20_000;

/** Replace a balanced `{…}` / `[…]` that starts a line or word and holds a
 *  JSON property (a string, optional whitespace, then `:`) with an ellipsis,
 *  so pretty-printed or spaced JSON never reaches the strip. One forward pass
 *  that tracks strings and escapes, so a delimiter inside a string does not
 *  count. A span that does not balance within the cap is kept as written and
 *  skipped, never rescanned. */
function scrubJson(text: string): string {
  let out = "";
  let i = 0;
  while (i < text.length) {
    const c = text[i];
    if ((c === "{" || c === "[") && (i === 0 || /\s/.test(text[i - 1]))) {
      const end = Math.min(text.length, i + JSON_SPAN_MAX);
      let depth = 0;
      let inStr = false;
      let esc = false;
      let afterStr = false;
      let prop = false;
      let j = i;
      for (; j < end; j++) {
        const d = text[j];
        if (inStr) {
          if (esc) esc = false;
          else if (d === "\\") esc = true;
          else if (d === '"') {
            inStr = false;
            afterStr = true;
          }
          continue;
        }
        if (d === " " || d === "\t" || d === "\n" || d === "\r") continue;
        if (d === '"') {
          inStr = true;
          afterStr = false;
          continue;
        }
        if (d === ":" && afterStr) prop = true;
        afterStr = false;
        if (d === "{" || d === "[") depth++;
        else if (d === "}" || d === "]") {
          depth--;
          if (depth === 0) {
            j++;
            break;
          }
        }
      }
      const span = text.slice(i, j);
      out += depth === 0 && prop ? "…" : span;
      i = j;
    } else {
      out += c;
      i++;
    }
  }
  return out;
}

/** The agent's own words: command lines (`bash:`, `$ `) dropped, and every
 *  path-like or inline-JSON word replaced by an ellipsis. Linear: one pass per
 *  line, one anchored test per word. */
function plainWords(text: string): string {
  return scrubJson(text)
    .split("\n")
    .filter((line) => !/^\s*(?:bash:|\$ )/i.test(line))
    .map((line) =>
      line
        .split(/(\s+)/)
        .map((w) => (PATHISH.test(w) || ((w[0] === "{" || w[0] === "[") && w.includes('":')) ? "…" : w))
        .join(""),
    )
    .join("\n");
}

/** Artifact ids a tool call asks to add a version to: `artifact put … --id art-x`. */
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
    let plan: string | null = null;
    const steps: WorkStep[] = [];
    const updating = new Set<string>();
    for (const item of tail) {
      if (item.type === "commentary" && own(item.entry.message) && item.entry.text.trim()) plan = plainWords(item.entry.text.trim());
      if (item.type === "tools") {
        for (const s of toolSteps(item.entries.filter((x) => own(x.message)))) {
          steps.push({ text: plainStep(s.call.text), raw: s.summary, done: s.done });
          for (const id of putTargets(s.call.text)) updating.add(id);
        }
      }
    }
    const tools = [...tail].reverse().find((i) => i.type === "tools");
    const open = tools && tools.type === "tools" ? openStep(tools.entries.filter((x) => own(x.message))) : null;
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
  running: string | null;
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
