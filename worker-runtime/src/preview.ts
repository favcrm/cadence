import type { AssistantMessage } from "@earendil-works/pi-ai";
import type {
  AgentEvent,
  MessageChange,
  SnapshotEvent,
} from "@earendil-works/pi-durable";

/**
 * The engine's committed assistant entry kind. A compaction summary is
 * assistant-shaped too, so only this kind's `message_end` retires the live
 * preview; a compaction committing under a running request must not.
 */
const ASSISTANT_ENTRY_KIND = "pi.assistant";

/**
 * Upper bound on the transient text one connection retains and may send.
 *
 * The committed transcript carries the whole answer, so a preview only needs
 * its beginning. A bound keeps the projector and every queued frame small
 * even when a model streams for a long time: the per-block state stops
 * growing once the budget is spent, and the joined preview is capped again.
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
 * One text block of the in-flight assistant message. Eligibility is decided
 * over the whole message text (see `text`), never per block: a safe prose
 * prefix and a token-bearing JSON suffix can share one message.
 */
type TextBlock = { readonly kind: "text"; text: string };

/** A block a preview never projects: thinking, tool calls, images. */
type OtherBlock = { readonly kind: "other" };

type BlockState = TextBlock | OtherBlock;

/**
 * Cap a preview without leaving half of a surrogate pair at the end.
 * `MAX_PREVIEW_CHARS` is a budget, not a wire-byte count: JSON escaping can
 * expand a control-heavy preview by a small constant factor.
 */
function capPreview(text: string): string {
  if (text.length <= MAX_PREVIEW_CHARS) return text;
  const cut = text.slice(0, MAX_PREVIEW_CHARS);
  const last = cut.charCodeAt(cut.length - 1);
  return last >= 0xd800 && last <= 0xdbff ? cut.slice(0, -1) : cut;
}

/** What one batch of engine events did to the preview. */
export interface PreviewBatch {
  /** The preview after the batch; `null` when no eligible text is in flight. */
  readonly current: string | null;
  /** The last eligible preview observed while applying the batch. */
  readonly latest: string | null;
}

/**
 * The transient projection of the in-flight assistant message.
 *
 * It is a pure function of the engine's events: the attached `snapshot`'s
 * generation partial seeds it, `message_start` replaces it, `message_update`
 * changes apply to it, and a committed assistant entry, a retry, a deferred
 * poll, a failed generation or a replacement snapshot retires it. Only genuine
 * assistant text is eligible; thinking, tool calls, arguments, provider
 * errors and raw event objects never contribute. Every emitted character
 * comes from the engine's own partial text, never from a synthesized value.
 */
export class ThreadPreview {
  private blocks = new Map<number, BlockState>();
  /**
   * Text budget left: `MAX_PREVIEW_CHARS - sum(retained text lengths)`.
   * Replacing or removing a text block returns its retained length here.
   */
  private remaining = MAX_PREVIEW_CHARS;
  /** The message the current blocks were seeded from, for a defensive retire. */
  private seeded: object | null = null;
  /** Set when the block bound was exceeded; no further preview until a seed. */
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
    let joined = "";
    // Content blocks concatenate directly: a token split across two blocks
    // must be seen as one string, not separated by presentation newlines.
    for (const index of [...this.blocks.keys()].sort((a, b) => a - b)) {
      const block = this.blocks.get(index);
      if (block !== undefined && block.kind === "text") joined += block.text;
    }
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
    this.blocks.clear();
    this.remaining = MAX_PREVIEW_CHARS;
    this.seeded = null;
    this.overflowed = false;
    this.quarantined = false;
    this.frozen = null;
  }

  /**
   * Stop previewing this message when the tracked-block bound is exceeded: a
   * partial projection would be misleading, while the committed entry will
   * arrive in full.
   */
  private failClosed(): void {
    this.overflowed = true;
    this.blocks.clear();
    this.remaining = 0;
    this.seeded = null;
    this.quarantined = false;
    this.frozen = null;
  }

  /**
   * Replace the preview with the in-flight partial of an attached snapshot.
   * The snapshot is authoritative: it is what a reconnection and an overflow
   * delivery both get, so an older incremental state never survives it.
   */
  applySnapshot(snapshot: SnapshotEvent): void {
    this.reset();
    const message = snapshot.generation?.message;
    if (message !== undefined) this.seed(message);
  }

  /**
   * Apply one batch of engine events and report what happened.
   *
   * `latest` is the last eligible text seen while applying the batch. A batch
   * that streams the final chunk and commits the message in one commit still
   * lets its caller show that text before the committed entry; nothing here
   * manufactures a frame when no eligible text existed.
   */
  apply(events: readonly AgentEvent[]): PreviewBatch {
    // A non-streamed message is emitted as `message_start` and `message_end`
    // for the same record in one batch. That record is committed history, not
    // in-flight text, so it must neither seed the preview nor be shown as a
    // transient message.
    const committed = new Set<object>();
    for (const event of events) {
      if (event.type !== "message_end") continue;
      const message = event.entry.model?.[0];
      if (message !== undefined && message.role === "assistant")
        committed.add(message);
    }
    let latest: string | null = null;
    for (const event of events) {
      if (!this.applyEvent(event, committed)) continue;
      const text = this.text;
      if (text !== null) latest = text;
    }
    return { current: this.text, latest };
  }

  private applyEvent(
    event: AgentEvent,
    committed: ReadonlySet<object>,
  ): boolean {
    switch (event.type) {
      case "snapshot":
        this.applySnapshot(event);
        return true;
      case "message_start":
        if (
          event.message.role === "assistant" &&
          !committed.has(event.message)
        ) {
          this.seed(event.message);
          return true;
        }
        return false;
      case "message_update": {
        let touched = false;
        for (const change of event.changes)
          touched = this.applyChange(change) || touched;
        return touched;
      }
      case "message_end": {
        const message = event.entry.model?.[0];
        if (
          message !== undefined &&
          (message === this.seeded ||
            event.entry.kind === ASSISTANT_ENTRY_KIND)
        ) {
          this.reset();
          return true;
        }
        return false;
      }
      // A retry waits for a fresh attempt and a deferred response has no
      // streaming body: neither has in-flight text any more.
      case "auto_retry_start":
      case "deferred_poll":
        this.reset();
        return true;
      case "task_failed":
        if (event.kind === "pi.generation") {
          this.reset();
          return true;
        }
        return false;
      default:
        // Committed entries (message_end above), tool progress, usage, inbox
        // and agent state changes never carry preview text.
        return false;
    }
  }

  private applyChange(change: MessageChange): boolean {
    switch (change.type) {
      case "text_start":
        if (change.block.type === "text")
          this.startText(change.contentIndex, change.block.text);
        else this.markOther(change.contentIndex);
        return true;
      case "text_delta":
        this.appendText(change.contentIndex, change.delta);
        return true;
      case "block":
        this.replaceBlock(change.contentIndex, change.block);
        return true;
      case "message":
        this.seed(change.message);
        return true;
      case "thinking_start":
      case "toolcall_start":
        // A non-text block never previews; replacing an index removes any
        // text it supersedes.
        this.markOther(change.contentIndex);
        return true;
      case "thinking_delta":
      case "toolcall_delta":
        // Thinking and tool arguments stay private, even as they stream.
        return false;
      default:
        return false;
    }
  }

  /** Replace the whole preview with one assistant message's text blocks. */
  private seed(message: AssistantMessage): void {
    this.reset();
    if (message.content.length > MAX_PREVIEW_BLOCKS) {
      this.failClosed();
      return;
    }
    this.seeded = message;
    for (const [index, block] of message.content.entries()) {
      if (block.type === "text") this.startText(index, block.text);
      else this.markOther(index);
    }
  }

  /**
   * Track a non-text block. It never contributes text, and the map's size is
   * bounded so arbitrarily many thinking/tool blocks cannot grow the state.
   */
  private markOther(index: number): void {
    if (this.overflowed || this.quarantined) return;
    const existing = this.blocks.get(index);
    if (existing === undefined && this.blocks.size >= MAX_PREVIEW_BLOCKS) {
      this.failClosed();
      return;
    }
    // A text block is about to disappear: return its retained length so the
    // budget invariant holds and later blocks can still use it.
    if (existing?.kind === "text")
      this.remaining = Math.min(
        MAX_PREVIEW_CHARS,
        this.remaining + existing.text.length,
      );
    this.blocks.set(index, { kind: "other" });
  }

  /** Start (or replace) one text block at `index` with its known text. */
  private startText(index: number, text: string): void {
    if (this.overflowed || this.quarantined) return;
    const existing = this.blocks.get(index);
    if (existing === undefined && this.blocks.size >= MAX_PREVIEW_BLOCKS) {
      this.failClosed();
      return;
    }
    // A `block` change re-reports the whole block. Return the text it already
    // retained before appending the replacement, or a full-length streamed
    // block followed by its block change would spend the budget twice and lose
    // the preview before the final entry.
    if (existing?.kind === "text")
      this.remaining = Math.min(
        MAX_PREVIEW_CHARS,
        this.remaining + existing.text.length,
      );
    this.blocks.set(index, { kind: "text", text: "" });
    this.appendText(index, text);
  }

  /** Replace one block wholesale, as a `block` change reports it. */
  private replaceBlock(
    index: number,
    block: AssistantMessage["content"][number],
  ): void {
    if (block.type === "text") this.startText(index, block.text);
    else this.markOther(index);
  }

  /**
   * Append text to one block while keeping the size bound. The whole-message
   * guard lives in `text`; this only accumulates, and stops once the message
   * is quarantined.
   */
  private appendText(index: number, delta: string): void {
    // Only a verified text block can take text. A delta for an unknown index
    // or for a thinking/tool block is ignored: a withheld block must never be
    // turned into visible text by a stray delta.
    const block = this.blocks.get(index);
    if (block === undefined || block.kind !== "text") return;
    if (this.quarantined || delta.length === 0) return;
    const room = this.remaining;
    if (room <= 0) return;
    const addition = delta.length <= room ? delta : delta.slice(0, room);
    block.text += addition;
    this.remaining -= addition.length;
  }
}
