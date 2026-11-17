/**
 * The board's explicit-activity signal. A person's pointer or key action marks
 * the next request; periodic metadata, banners, health polls, retries and
 * background reconnects never carry it, so presence cannot extend a session.
 *
 * One gesture arms exactly one request. The flag is consumed on first use, so a
 * timer firing a moment after a click cannot take the mark instead of the
 * request the click caused.
 */
const WINDOW_MS = 1_500;

let armedAt = 0;
let consumed = true;

function mark(): void {
  armedAt = Date.now();
  consumed = false;
}

if (typeof window !== "undefined") {
  window.addEventListener("pointerdown", mark, { capture: true, passive: true });
  window.addEventListener("keydown", mark, { capture: true, passive: true });
}

/** Headers for the one request the next user gesture justifies. */
export function activityHeaders(): Record<string, string> {
  if (consumed || Date.now() - armedAt > WINDOW_MS) return {};
  consumed = true;
  return { "X-Cadence-Activity": "user" };
}
