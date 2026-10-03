import type { FrameSize } from "./contract";
import type { Directive } from "./directive";

/**
 * Which chat rows show a live sandboxed frame (CAD-1110, Tier 2). One live
 * frame per tag, at most `MAX_FRAMES` per pane: the frame lives at the FIRST
 * row that names the tag and every later directive for it is pushed into
 * that same frame (push-only); the later rows read "shown above". A tag past
 * the limit, least recently updated first, reads "Preview closed". A tag
 * that failed to mount falls back to plain text for every row naming it.
 */
export const MAX_FRAMES = 3;

type FrameDirective = Extract<Directive, { kind: "frame" }>;
export type FrameRow =
  | { state: "live"; tag: string; size: FrameSize; match: string; data: FrameDirective["data"] }
  | { state: "updated"; tag: string }
  | { state: "closed"; tag: string };

export function planFrames(
  rows: { key: string; directive: Directive | null }[],
  failed: ReadonlySet<string>,
): Map<string, FrameRow> {
  const byTag = new Map<string, { keys: string[]; last: number; directive: FrameDirective }>();
  rows.forEach((row, index) => {
    const d = row.directive;
    if (!d || d.kind !== "frame" || failed.has(d.tag)) return;
    const entry = byTag.get(d.tag);
    if (entry) {
      entry.keys.push(row.key);
      entry.last = index;
      entry.directive = d;
    } else {
      byTag.set(d.tag, { keys: [row.key], last: index, directive: d });
    }
  });
  const live = [...byTag.entries()].sort((a, b) => b[1].last - a[1].last).slice(0, MAX_FRAMES).map(([tag]) => tag);
  const plan = new Map<string, FrameRow>();
  for (const [tag, entry] of byTag) {
    const isLive = live.includes(tag);
    entry.keys.forEach((key, i) => {
      if (!isLive) plan.set(key, { state: "closed", tag });
      else if (i === 0) {
        const d = entry.directive;
        plan.set(key, { state: "live", tag, size: d.size, match: d.match, data: d.data });
      } else plan.set(key, { state: "updated", tag });
    });
  }
  return plan;
}
