import { ApiError } from "../../lib/api";
import { hostErrorText } from "./shared/hostErrors";

/**
 * The approved saved-segment rule grammar (CAD-780 allowlist,
 * CAD-784 screens). Four fields, two operators, Rust-evaluated on
 * the host — predicate values never become SQL. Client checks mirror
 * the daemon's bounds so malformed rules refuse before the wire;
 * the server stays the source of truth and its refusals surface
 * verbatim through `friendlyError`.
 */

export const SEGMENT_FIELDS = [
  { value: "tag", label: "Tag", hint: "customers carrying this tag (letters, digits, - _)" },
  { value: "source", label: "Source", hint: "the import source label (letters, digits, - _)" },
  {
    value: "consent_email",
    label: "Email consent",
    hint: "granted, denied or unknown",
  },
  {
    value: "email_domain",
    label: "Email domain",
    hint: "the part after @, lowercased (example.com)",
  },
] as const;

export type SegmentField = (typeof SEGMENT_FIELDS)[number]["value"];

export const SEGMENT_OPS = [
  { value: "eq", label: "is" },
  { value: "ne", label: "is not" },
] as const;

export type SegmentOp = (typeof SEGMENT_OPS)[number]["value"];

export function isSegmentField(value: string): value is SegmentField {
  return (SEGMENT_FIELDS as readonly { value: string }[]).some((field) => field.value === value);
}

export function isSegmentOp(value: string): value is SegmentOp {
  return (SEGMENT_OPS as readonly { value: string }[]).some((op) => op.value === value);
}

function tagShape(value: string): boolean {
  return (
    value.length > 0 &&
    value.length <= 40 &&
    /^[A-Za-z0-9_-]+$/.test(value)
  );
}

function domainShape(value: string): boolean {
  const domain = value.toLowerCase();
  return (
    domain.includes(".") &&
    /^[a-z0-9.-]+$/.test(domain)
  );
}

/** Client mirror of the daemon's predicate grammar. Refusals name
 *  the shape, never the value. */
export function checkPredicate(field: string, op: string, value: string): void {
  if (!isSegmentField(field) || !isSegmentOp(op)) {
    throw new ApiError("segment rule uses a field or operator outside the approved grammar", 400);
  }
  if (
    value.length === 0 ||
    value.length > 120 ||
    [...value].some((ch) => ch.charCodeAt(0) < 32 || ch.charCodeAt(0) === 127)
  ) {
    throw new ApiError("segment rule value exceeds its supported shape or bounds", 400);
  }
  if (
    value.includes("<") ||
    value.includes(">") ||
    value.includes("'") ||
    value.includes('"') ||
    value.includes(";") ||
    value.includes("\\") ||
    value.includes("--") ||
    value.toLowerCase().includes("select ")
  ) {
    throw new ApiError("segment rule value exceeds its supported grammar", 400);
  }
  if (field === "tag" || field === "source") {
    if (!tagShape(value)) {
      throw new ApiError(`segment rule value is not a valid ${field}`, 400);
    }
  } else if (field === "consent_email") {
    if (value !== "granted" && value !== "denied" && value !== "unknown") {
      throw new ApiError("email consent is granted, denied or unknown", 400);
    }
  } else if (!domainShape(value)) {
    throw new ApiError("email domain needs a dotted host such as example.com", 400);
  }
}

export function checkSegmentName(name: string): void {
  if (name.length === 0 || name.length > 80 || name.trim() !== name) {
    throw new ApiError("segment name is required (80 characters, no padding)", 400);
  }
  if ([...name].some((ch) => ch.charCodeAt(0) < 32 || ch.charCodeAt(0) === 127)) {
    throw new ApiError("segment name carries unsupported characters", 400);
  }
}

/** Random stable segment/list IDs in the daemon's identifier grammar. */
export function newAudienceId(prefix: string): string {
  const bytes = new Uint8Array(9);
  try {
    crypto.getRandomValues(bytes);
  } catch {
    for (let i = 0; i < bytes.length; i++) bytes[i] = Math.floor(Math.random() * 256);
  }
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
  return `${prefix}-${hex.slice(0, 12)}`;
}

/** Server refusals stay generic; transport failures name the retry. */
export function friendlyAudienceError(error: unknown): string {
  return hostErrorText(error, "The audience request was refused — retry.");
}
