import { FORBIDDEN_DESCRIPTOR_KEYS } from "../app-views/contract";
import { CHAT_PUSH_BYTES_MAX } from "../../workspace-apps/screen/screenProtocol";
import type { AppChat, DirectiveCardSpec, FrameSize } from "./contract";

/**
 * The host's directive matcher and payload scan (CAD-1109 rules 1-5). A
 * directive is an ordinary thread message whose whole text is a JSON object.
 * It can only choose WHICH declared card or screen appears and fill that
 * card's declared text fields: it never adds a button, an action, an id or a
 * link. A message that fails any rule is refused whole (plain text), never
 * partially rendered.
 */

export type Leaf = string | number | boolean;
export type Payload = Record<string, Leaf>;

export type Directive =
  | { kind: "card"; match: string; card: DirectiveCardSpec; fields: { label: string; value: string }[] }
  | { kind: "frame"; match: string; tag: string; size: FrameSize; data: Payload }
  /** Guard 10: a token-bearing message with no declared directive. */
  | { kind: "confirmation" };

const MAX_TEXT_BYTES = 4096;
const MAX_KEYS = 16;
const MAX_LEAF = 512;
const MAX_FIELD_VALUE = 120;
/** Keys a field-bearing card or any screen directive refuses outright
 *  (rule 5): the descriptor's forbidden keys plus identity, secret and
 *  button-shaped names. */
const REFUSED = new Set<string>([
  ...FORBIDDEN_DESCRIPTOR_KEYS,
  "record_id", "request_id", "confirm_token", "id",
  "run", "view", "buttons",
]);

function isPlain(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v)
    && (Object.getPrototypeOf(v) === Object.prototype || Object.getPrototypeOf(v) === null);
}
const byteLength = (s: string) => new TextEncoder().encode(s).byteLength;

/** Rules 1-2: the message text is one JSON object with exactly one own key. */
function oneKey(text: string): { key: string; value: unknown } | null {
  const body = text.trim();
  if (!body.startsWith("{") || byteLength(body) > MAX_TEXT_BYTES) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    return null;
  }
  if (!isPlain(parsed)) return null;
  const keys = Object.keys(parsed);
  if (keys.length !== 1) return null;
  const desc = Object.getOwnPropertyDescriptor(parsed, keys[0]);
  if (!desc || desc.get || desc.set) return null;
  return { key: keys[0], value: desc.value };
}

/** Rule 4: a flat object of at most 16 keys, leaves string (<= 512 chars),
 *  finite number or boolean; nothing nested. */
function flat(value: unknown): Payload | null {
  if (!isPlain(value)) return null;
  const keys = Object.keys(value);
  if (keys.length > MAX_KEYS) return null;
  const out: Payload = {};
  for (const key of keys) {
    const desc = Object.getOwnPropertyDescriptor(value, key);
    if (!desc || desc.get || desc.set) return null;
    const leaf = desc.value;
    if (typeof leaf === "string" ? leaf.length > MAX_LEAF
      : typeof leaf === "number" ? !Number.isFinite(leaf)
      : typeof leaf !== "boolean") return null;
    out[key] = leaf;
  }
  return out;
}

/** Rule 5 (identity half): no refused key anywhere in the flat payload. */
const clean = (data: Payload) => Object.keys(data).every((k) => !REFUSED.has(k));
/** Action-shaped keys no payload may carry, not even an opaque card's: the
 *  button list is the descriptor's alone, so a payload that tries to name an
 *  action is refused to plain text rather than ignored. */
const ACTION_KEYS = ["action", "run", "view", "buttons"];
const actionShaped = (data: Payload) => ACTION_KEYS.some((k) => Object.hasOwn(data, k));

const SECRET_KEYS = new Set(["confirm_token", "request_id"]);
/** Guard 10: a JSON object carrying `confirm_token` or `request_id` at any
 *  depth. Bounded walk: an adversarially deep or wide body is treated as
 *  carrying one (it is never printed raw). */
function carriesSecret(text: string): boolean {
  const body = text.trim();
  if (!body.startsWith("{")) return false;
  let parsed: unknown;
  try {
    parsed = JSON.parse(body);
  } catch {
    return false;
  }
  if (!isPlain(parsed)) return false;
  let budget = 4096;
  const walk = (v: unknown, depth: number): boolean => {
    if (--budget < 0 || depth > 64) return true;
    if (Array.isArray(v)) return v.some((x) => walk(x, depth + 1));
    if (typeof v !== "object" || v === null) return false;
    return Object.entries(v).some(([k, x]) => SECRET_KEYS.has(k) || walk(x, depth + 1));
  };
  return walk(parsed, 0);
}

/**
 * The directive a message text is, against THIS pane's own installation
 * descriptor (`null` = plain shared chat: nothing matches), or `null` when
 * it renders as ordinary text. `home` panes never call this.
 */
export function matchDirective(text: string, descriptor: AppChat | null): Directive | null {
  const one = oneKey(text);
  const spec = one && descriptor?.directives.find((d) => d.match === one.key);
  if (one && spec) {
    const data = flat(one.value);
    if (data) {
      if (spec.render !== null) {
        // A screen directive has no opaque form: its payload is always
        // scanned, and the whole push stays under 4 KiB.
        const push = JSON.stringify({ v: 1, op: "directive", tag: spec.render, kind: spec.match, data });
        if (clean(data) && byteLength(push) <= CHAT_PUSH_BYTES_MAX) {
          return { kind: "frame", match: spec.match, tag: spec.render, size: spec.size, data };
        }
      } else if (spec.card.fields.length === 0) {
        // Opaque card: shape-checked above, its values never read.
        if (!actionShaped(data)) return { kind: "card", match: spec.match, card: spec.card, fields: [] };
      } else if (
        clean(data) &&
        spec.card.fields.every((f) => Object.hasOwn(data, f.from))
      ) {
        const fields = spec.card.fields.map((f) => ({
          label: f.label,
          value: String(data[f.from]).slice(0, MAX_FIELD_VALUE),
        }));
        return { kind: "card", match: spec.match, card: spec.card, fields };
      }
    }
  }
  return carriesSecret(text) ? { kind: "confirmation" } : null;
}

/** Internal context ids (`ctx-…`) are plumbing, not copy (host constant, D4). */
export function hideIds(text: string): string {
  return text.replace(/\bctx-[A-Za-z0-9_-]+/g, "this workspace");
}
