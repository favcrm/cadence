import type { IssueCard } from "./types";

const REQUEST_ID = /^[A-Za-z0-9_-]{1,64}$/;

/** DTO-only error shared with screens; no transport, session or API singleton. */
export class ApiError extends Error {
  status: number;
  conflict?: string;
  check?: string;
  code?: string;
  revision?: number | null;
  card?: IssueCard;
  requestId?: string;
  constructor(
    message: string,
    status: number,
    opts?: {
      conflict?: string;
      check?: string;
      code?: string;
      revision?: number | null;
      card?: IssueCard;
      requestId?: string;
    },
  ) {
    super(message);
    this.status = status;
    this.conflict = opts?.conflict;
    this.check = opts?.check;
    this.code = opts?.code;
    this.revision = opts?.revision;
    this.card = opts?.card;
    this.requestId = opts?.requestId;
  }
}

/** Bounded correlation request_id from a JSON error body or `x-request-id` header. */
export function errorRequestId(body: unknown, response?: Response): string | undefined {
  if (body !== null && typeof body === "object") {
    const err = "error" in body && body.error !== null && typeof body.error === "object"
      ? (body.error as Record<string, unknown>)
      : (body as Record<string, unknown>);
    if (typeof err.request_id === "string" && REQUEST_ID.test(err.request_id)) {
      return err.request_id;
    }
  }
  const header = response?.headers?.get?.("x-request-id")?.trim();
  return header && REQUEST_ID.test(header) ? header : undefined;
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
