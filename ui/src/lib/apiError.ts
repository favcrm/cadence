import type { IssueCard } from "./types";

/** DTO-only error shared with screens; no transport, session or API singleton. */
export class ApiError extends Error {
  status: number;
  conflict?: string;
  check?: string;
  code?: string;
  revision?: number | null;
  card?: IssueCard;
  constructor(
    message: string,
    status: number,
    opts?: {
      conflict?: string;
      check?: string;
      code?: string;
      revision?: number | null;
      card?: IssueCard;
    },
  ) {
    super(message);
    this.status = status;
    this.conflict = opts?.conflict;
    this.check = opts?.check;
    this.code = opts?.code;
    this.revision = opts?.revision;
    this.card = opts?.card;
  }
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
