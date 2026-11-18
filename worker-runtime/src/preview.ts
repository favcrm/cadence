import type { AssistantMessage } from "@earendil-works/pi-ai";
import type { LiveState } from "@earendil-works/pi-durable";

/**
 * Upper bound on the transient text one connection retains and may send.
 *
 * The committed transcript carries the whole answer, so a preview only needs
 * its beginning. A bound keeps the projector and every queued frame small
 * even when a model streams for a long time: retained text stops growing once
 * the budget is spent.
 */
export const MAX_PREVIEW_CHARS = 8192;

/**
 * How many content blocks one in-flight message may track.
 *
 * The character budget bounds the text, not the number of empty thinking,
 * tool-call or text blocks a provider can report. Exceeding this bound drops
 * the in-flight preview entirely: the committed entries stay authoritative,
 * and the next message or snapshot starts a clean projection.
 */
export const MAX_PREVIEW_BLOCKS = 256;

/**
 * Keys the board treats as token-bearing (`directive.ts`'s `SECRET_KEYS`).
 * A preview must never show one, whether it sits inside a JSON object or in
 * bare text, and whether a provider splits it across chunks or blocks.
 */
const SENSITIVE_KEYS = [
  { key: "confirm_token", pattern: /confirm_token/i },
  { key: "request_id", pattern: /request_id/i },
] as const;

/**
 * Cap a preview without leaving half of a surrogate pair at the end.
 * `MAX_PREVIEW_CHARS` is a budget, not a wire-byte count: JSON escaping can
 * expand a control-heavy preview by a small constant factor.
 */
function capPreview(text: string): string {
  const cut = text.length <= MAX_PREVIEW_CHARS ? text : text.slice(0, MAX_PREVIEW_CHARS);
  const last = cut.charCodeAt(cut.length - 1);
  return last >= 0xd800 && last <= 0xdbff ? cut.slice(0, -1) : cut;
}

/**
 * Project only text from an in-flight assistant message. Quarantine is sticky
 * for the generation, even when later view updates replace earlier content.
 */
export class ThreadPreview {
  private joined = "";
  /** Set when the block bound was exceeded; no further preview until reset. */
  private overflowed = false;
  /** Set when the message revealed a `{` or a sensitive key. Sticky. */
  private quarantined = false;
  /** The provably safe prefix captured at quarantine time. */
  private frozen: string | null = null;

  /**
   * The provably safe in-flight text, or `null` when there is none.
   *
   * The whole message is scanned before anything is emitted: a `{` anywhere
   * may open a host directive or a token-bearing confirmation, and the keys
   * `confirm_token` / `request_id` are withheld even outside an object. Once
   * either appears the prefix before it is frozen for the rest of the
   * message; later blocks or replacements never release the withheld suffix.
   * A trailing text suffix that could still complete one of those keys is
   * held back until it diverges or completes.
   */
  get text(): string | null {
    if (this.overflowed) return null;
    if (this.quarantined) return this.frozen;
    // Blocks concatenate directly, so a key split across them is still seen.
    const joined = this.joined;
    if (joined.length === 0) return null;
    let stop = joined.length;
    let unsafe = false;
    const brace = joined.indexOf("{");
    if (brace >= 0) {
      stop = brace;
      unsafe = true;
    }
    // Match case-insensitively against the ORIGINAL text. `toLowerCase` can
    // change UTF-16 length (e.g. U+0130), so an index into a lowered copy
    // would not address `joined` and could miss the key entirely.
    for (const { pattern } of SENSITIVE_KEYS) {
      const at = joined.search(pattern);
      if (at >= 0 && at < stop) {
        stop = at;
        unsafe = true;
      }
    }
    for (const { key } of SENSITIVE_KEYS) {
      const most = Math.min(key.length - 1, joined.length);
      for (let n = most; n > 0; n--) {
        // The keys are ASCII, so lowercasing only the short original suffix is
        // safe: a non-ASCII expansion can never equal an ASCII key prefix.
        const start = joined.length - n;
        if (joined.slice(start).toLowerCase() !== key.slice(0, n)) continue;
        if (start < stop) stop = start;
        break;
      }
    }
    const safe = capPreview(joined.slice(0, stop));
    if (unsafe) {
      // Sticky: never release the quarantined suffix during this message,
      // even if later blocks or replacements would read as prose.
      this.quarantined = true;
      this.frozen = safe.length === 0 ? null : safe;
      return this.frozen;
    }
    return safe.length === 0 ? null : safe;
  }

  /** Forget the current message. A reconnection starts from its snapshot. */
  reset(): void {
    this.joined = "";
    this.overflowed = false;
    this.quarantined = false;
    this.frozen = null;
  }

  /** Replace the retained text with the latest engine-owned partial. */
  update(message: AssistantMessage | undefined): string | null {
    if (message === undefined) {
      this.joined = "";
      return null;
    }
    if (this.overflowed || this.quarantined) return this.text;
    if (message.content.length > MAX_PREVIEW_BLOCKS) {
      this.joined = "";
      this.overflowed = true;
      return null;
    }
    let joined = "";
    for (const block of message.content) {
      if (block.type !== "text") continue;
      const room = MAX_PREVIEW_CHARS - joined.length;
      if (room <= 0) break;
      joined += block.text.length <= room ? block.text : block.text.slice(0, room);
    }
    this.joined = joined;
    return this.text;
  }
}

/**
 * Records every unfolded engine publication before the async stream can fold
 * revisions. A quarantine survives replacements within the same generation.
 */
export class PreviewGuard {
  private readonly preview = new ThreadPreview();
  private taskId: number | undefined;
  private attempt: number | undefined;
  private firstInput: number | undefined;
  private current: string | null = null;
  private attached = false;
  private early = false;
  private blocked = false;

  private matches(live: LiveState | undefined): boolean {
    return this.taskId === live?.run?.taskId &&
      this.attempt === live?.generation?.attempt &&
      this.firstInput === live?.run?.inputs[0];
  }

  /** Called synchronously for each publication, without any engine reads. */
  advance(live: LiveState | undefined): void {
    if (!this.attached) this.early = true;
    if (!this.matches(live)) {
      this.preview.reset();
      this.blocked = false;
      this.taskId = live?.run?.taskId;
      this.attempt = live?.generation?.attempt;
      this.firstInput = live?.run?.inputs[0];
    }
    this.current = this.preview.update(live?.generation?.message);
  }

  /** Complete the atomic attachment without overwriting an early publication. */
  attach(live: LiveState | undefined): void {
    if (this.early) this.blocked = true;
    else this.advance(live);
    this.attached = true;
  }

  /** Never project a different or ambiguously observed generation. */
  text(live: LiveState | undefined): string | null {
    if (!this.attached || this.blocked || !this.matches(live)) return null;
    return this.current;
  }
}
