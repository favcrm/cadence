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
