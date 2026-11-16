import type { IssueCard } from "./types";

/** DTO-only error shared with screens; no transport, session or API singleton. */
export class ApiError extends Error {
  status: number;
  conflict?: string;
  check?: string;
  code?: string;
  revision?: number | null;
  retryAfter?: number | null;
  card?: IssueCard;
  constructor(
    message: string,
    status: number,
    opts?: {
      conflict?: string;
      check?: string;
      code?: string;
      revision?: number | null;
      retryAfter?: number | null;
      card?: IssueCard;
    },
  ) {
    super(message);
    this.status = status;
    this.conflict = opts?.conflict;
    this.check = opts?.check;
    this.code = opts?.code;
    this.revision = opts?.revision;
    this.retryAfter = opts?.retryAfter;
    this.card = opts?.card;
  }
}

function positiveSeconds(raw: unknown): number | null {
  if (typeof raw === "number") {
    return Number.isFinite(raw) && raw > 0 ? Math.min(86_400, Math.ceil(raw)) : null;
  }
  if (typeof raw === "string" && /^\s*\d+(?:\.\d+)?\s*$/.test(raw)) {
    const num = Number(raw);
    return Number.isFinite(num) && num > 0 ? Math.min(86_400, Math.ceil(num)) : null;
  }
  return null;
}

/** Extract positive retry-after delay in seconds from JSON error fields or `Retry-After` header. */
export function parseRetryAfterSeconds(body: unknown, headerValue?: string | null): number | null {
  if (body !== null && typeof body === "object" && !Array.isArray(body)) {
    const record = body as Record<string, unknown>;
    const nested =
      record.error !== null && typeof record.error === "object" && !Array.isArray(record.error)
        ? (record.error as Record<string, unknown>)
        : null;
    for (const candidate of [
      record.retry_after,
      record.retryAfterSeconds,
      nested?.retry_after,
      nested?.retryAfterSeconds,
    ]) {
      const secs = positiveSeconds(candidate);
      if (secs !== null) return secs;
    }
  }
  if (typeof headerValue === "string" && /^\s*\d+\s*$/.test(headerValue)) {
    return positiveSeconds(headerValue);
  }
  return null;
}

/** The server's error text, whether it answered with a bare string or with
 *  `{ error: { code, message } }`. Always a STRING: an `Error.message` that
 *  holds an object renders as "[object Object]", so the surface says nothing
 *  about what actually refused. */
export function errorMessage(body: unknown, fallback: string): string {
  if (body === null || typeof body !== "object" || !("error" in body)) return fallback;
  const error = body.error;
  if (typeof error === "string") return error === "" ? fallback : error;
  if (error !== null && typeof error === "object" && "message" in error) {
    const message = error.message;
    if (typeof message === "string" && message !== "") return message;
  }
  return fallback;
}
