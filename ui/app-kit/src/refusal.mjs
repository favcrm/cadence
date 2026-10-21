/**
 * Refusal codes are the contract; text is for the person reading the screen.
 * A reply that is neither `ok` nor a well-formed `{code, text}` refusal is a
 * protocol error, never a guessed status.
 */
export const REFUSAL_STATUS = Object.freeze({
  invalid: 400,
  refused: 400,
  unauthorized: 401,
  forbidden: 403,
  not_found: 404,
  missing: 404,
  stale_revision: 409,
  conflict: 409,
  unavailable: 503,
  timeout: 504,
});

const CODE = /^[a-z][a-z0-9_]{0,63}$/;
const MAX_TEXT = 2000;

export class ScreenRefusalError extends Error {
  constructor(refusal, statuses = REFUSAL_STATUS) {
    const code = typeof refusal?.code === "string" ? refusal.code : "invalid";
    const text = typeof refusal?.text === "string" ? refusal.text : "";
    super(text || code);
    this.name = "ScreenRefusalError";
    this.code = CODE.test(code) ? code : "invalid";
    this.text = text;
    this.status = statuses[this.code] ?? 400;
  }
}

export class ScreenTimeoutError extends Error {
  constructor(id, timeoutMs) {
    super(`no reply for ${id} within ${timeoutMs}ms`);
    this.name = "ScreenTimeoutError";
    this.code = "timeout";
    this.status = 504;
    this.id = id;
  }
}

/** A refusal record the host sent, or `null` when the shape is malformed. */
export function readRefusal(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  if (typeof value.code !== "string" || !CODE.test(value.code)) return null;
  if (typeof value.text !== "string" || value.text.length > MAX_TEXT) return null;
  return { code: value.code, text: value.text };
}
