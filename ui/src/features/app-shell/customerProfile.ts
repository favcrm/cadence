import { ApiError } from "../../lib/api";
import type { HostRecord } from "./hostActions";

/**
 * Allowlisted customer profile shaping for the CRM Customers screens
 * (CAD-781). The wire grammar stays exactly `{record_id, profile}` on
 * create and `{expected_revision, profile}` on update — this module
 * only shapes the `profile` value, mirroring the daemon's bounds so
 * refusals explain the field, never the content. No value is ever
 * echoed into an error: every message names the shape.
 */

export type ConsentChoice = "granted" | "denied" | "unknown";

export interface CustomerFormFields {
  displayName: string;
  email: string;
  phone: string;
  /** Raw tag text; separators are commas, semicolons or whitespace. */
  tags: string;
  source: string;
  consentEmail: ConsentChoice;
  consentSms: ConsentChoice;
}

export const EMPTY_CUSTOMER_FORM: CustomerFormFields = {
  displayName: "",
  email: "",
  phone: "",
  tags: "",
  source: "",
  consentEmail: "unknown",
  consentSms: "unknown",
};

export function isConsentChoice(value: string): value is ConsentChoice {
  return value === "granted" || value === "denied" || value === "unknown";
}

function hasControl(value: string): boolean {
  // eslint-disable-next-line no-control-regex
  return /[\u0000-\u001f\u007f]/.test(value);
}

function tagValid(tag: string): boolean {
  return (
    tag.length >= 1 && tag.length <= 40 && /^[A-Za-z0-9_-]+$/.test(tag)
  );
}

/** Split raw tag text; throws a field-named ApiError, never an echo. */
export function parseTags(raw: string): string[] {
  const tags = raw
    .split(/[,\s;]+/)
    .map((tag) => tag.trim())
    .filter((tag) => tag.length > 0);
  if (tags.length > 16) {
    throw new ApiError("Tags hold at most 16 entries", 400);
  }
  for (const tag of tags) {
    if (!tagValid(tag)) {
      throw new ApiError(
        "Tags use letters, digits, dash or underscore (1–40 characters each)",
        400,
      );
    }
  }
  if (new Set(tags).size !== tags.length) {
    throw new ApiError("Tags must not repeat", 400);
  }
  return tags;
}

function emailShapeValid(email: string): boolean {
  if (email.length === 0 || email.length > 254 || hasControl(email)) return false;
  if (/\s/.test(email)) return false;
  const parts = email.split("@");
  if (parts.length !== 2) return false;
  const [user, domain] = parts;
  return user.length > 0 && domain.length > 0 && domain.includes(".");
}

function phoneShapeValid(phone: string): boolean {
  if (phone.length < 7 || phone.length > 24) return false;
  const digits = phone.replace(/[^0-9]/g, "").length;
  return digits >= 7 && /^[0-9+ \-().]+$/.test(phone);
}

/**
 * Build the allowlisted `profile` value for create/update. Throws a
 * field-named ApiError on the first bad field. Consent is explicit by
 * construction: the form always sends the operator-visible choice,
 * defaulting to `unknown` — never inferred as granted.
 */
export function buildCustomerProfile(fields: CustomerFormFields): Record<string, unknown> {
  const displayName = fields.displayName;
  if (
    displayName.length === 0 ||
    displayName.length > 120 ||
    displayName.trim() !== displayName ||
    hasControl(displayName)
  ) {
    throw new ApiError("Display name is required (1–120 characters, no surrounding spaces)", 400);
  }
  const email = fields.email.trim();
  if (email !== "" && !emailShapeValid(email)) {
    throw new ApiError("Email needs one @ and a dotted domain", 400);
  }
  const phone = fields.phone.trim();
  if (phone !== "" && !phoneShapeValid(phone)) {
    throw new ApiError("Phone needs at least 7 digits (7–24 characters)", 400);
  }
  const source = fields.source.trim();
  if (source !== "" && !tagValid(source)) {
    throw new ApiError("Source uses letters, digits, dash or underscore (up to 40 characters)", 400);
  }
  const tags = parseTags(fields.tags);
  if (!isConsentChoice(fields.consentEmail) || !isConsentChoice(fields.consentSms)) {
    throw new ApiError("Consent must be granted, denied or unknown", 400);
  }
  const profile: Record<string, unknown> = {
    schema: 1,
    display_name: displayName,
    tags,
    consent: { email: fields.consentEmail, sms: fields.consentSms },
  };
  if (email !== "") profile.email = email;
  if (phone !== "") profile.phone = phone;
  if (source !== "") profile.source = source;
  return profile;
}

/** A fresh record id in the peer's segment grammar. Never derived from content. */
export function newCustomerId(): string {
  const bytes = new Uint8Array(9);
  try {
    crypto.getRandomValues(bytes);
  } catch {
    for (let i = 0; i < bytes.length; i++) bytes[i] = Math.floor(Math.random() * 256);
  }
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
  return `cust-${hex.slice(0, 12)}`;
}

export interface CustomerView {
  displayName: string;
  email: string | null;
  phone: string | null;
  tags: string[];
  source: string | null;
  consentEmail: string;
  consentSms: string | null;
}

/** Defensive read of an untrusted profile value; never throws. */
export function viewProfile(profile: unknown): CustomerView {
  const row =
    profile !== null && typeof profile === "object"
      ? (profile as Record<string, unknown>)
      : {};
  const text = (value: unknown): string | null =>
    typeof value === "string" && value.length > 0 ? value : null;
  const consent =
    row.consent !== null && typeof row.consent === "object"
      ? (row.consent as Record<string, unknown>)
      : {};
  return {
    displayName: text(row.display_name) ?? "—",
    email: text(row.email),
    phone: text(row.phone),
    tags: Array.isArray(row.tags) ? row.tags.filter((t): t is string => typeof t === "string") : [],
    source: text(row.source),
    consentEmail: text(consent.email) ?? "unknown",
    consentSms: text(consent.sms),
  };
}

/** Seed a form from a loaded profile for expected-revision edits. */
export function formFromProfile(profile: unknown): CustomerFormFields {
  const view = viewProfile(profile);
  return {
    displayName: view.displayName === "—" ? "" : view.displayName,
    email: view.email ?? "",
    phone: view.phone ?? "",
    tags: view.tags.join(", "),
    source: view.source ?? "",
    consentEmail: isConsentChoice(view.consentEmail) ? view.consentEmail : "unknown",
    consentSms: view.consentSms !== null && isConsentChoice(view.consentSms) ? view.consentSms : "unknown",
  };
}

export interface RevisionEntry {
  revision: number;
  digest: string;
  actor: string;
  at: number;
}

/** Attributed revision history; malformed entries are dropped, never rendered raw. */
export function revisionEntries(record: HostRecord): RevisionEntry[] {
  if (!Array.isArray(record.history)) return [];
  const entries: RevisionEntry[] = [];
  for (const item of record.history) {
    if (item === null || typeof item !== "object") continue;
    const row = item as Record<string, unknown>;
    if (
      typeof row.revision !== "number" ||
      typeof row.digest !== "string" ||
      typeof row.actor !== "string" ||
      typeof row.at !== "number"
    ) {
      continue;
    }
    entries.push({ revision: row.revision, digest: row.digest, actor: row.actor, at: row.at });
  }
  return entries.sort((a, b) => a.revision - b.revision);
}

export interface ConsentEntry {
  revision: number;
  channel: string;
  state: string;
  actor: string;
  at: number;
}

/** Per-channel consent/suppression transitions; malformed entries are dropped. */
export function consentEntries(record: HostRecord): ConsentEntry[] {
  if (!Array.isArray(record.consentHistory)) return [];
  const entries: ConsentEntry[] = [];
  for (const item of record.consentHistory) {
    if (item === null || typeof item !== "object") continue;
    const row = item as Record<string, unknown>;
    if (
      typeof row.revision !== "number" ||
      typeof row.channel !== "string" ||
      typeof row.state !== "string" ||
      typeof row.actor !== "string" ||
      typeof row.at !== "number"
    ) {
      continue;
    }
    entries.push({
      revision: row.revision,
      channel: row.channel,
      state: row.state,
      actor: row.actor,
      at: row.at,
    });
  }
  return entries.sort((a, b) => a.revision - b.revision);
}

/**
 * Operator-facing copy for a peer refusal. Server messages are generic
 * by design and safe to show; known shapes gain a next step. The input
 * value never appears here.
 */
export function friendlyError(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  if (/stale|expected_revision|expected record revision/i.test(message)) {
    return `${message} — someone else saved this record first. Reload and reapply your change.`;
  }
  if (/already holds different content|already used by another record/i.test(message)) {
    return `${message} — pick a different id or address, or open the existing record.`;
  }
  if (/supported shape or bounds/i.test(message)) {
    return `${message} — check display name, email, phone, tags and source against the field hints.`;
  }
  if (/refused app record field/i.test(message)) {
    return `${message} — the client refused a field the server never accepts.`;
  }
  return message;
}
