/**
 * CAD-600 units: the panel's scroll-follow math — when the thread is
 * "at the tail" (so new messages follow), where the tail sits, and the
 * jump pill's words.
 */
import { atTail, jumpLabel, tailTop, TAIL_SLACK } from "../src/features/home/dock";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function ok(cond: boolean, what: string): void {
  if (!cond) throw new Error(`failed: ${what}`);
}

// ---- atTail ----

// Exactly at the bottom.
ok(atTail(600, 1000, 400), "at the bottom is at the tail");
// Within the slack (48): a message still entering, a smooth scroll
// mid-flight, a rounding hair.
ok(atTail(560, 1000, 400), "48px above the bottom still counts");
ok(atTail(553, 1000, 400), "47px above the bottom still counts");
// Past the slack: the reader is up in the history.
ok(!atTail(540, 1000, 400), "60px above the bottom is away");
ok(!atTail(0, 1000, 400), "the top is away");
// Content that fits: there is no bottom to be away from.
ok(atTail(0, 400, 400), "a fitting thread is at its tail");
ok(atTail(0, 200, 400), "a short thread is at its tail");
// A custom slack.
ok(atTail(100, 1000, 400, 600), "a wide slack reaches further");
ok(!atTail(100, 1000, 400, 400), "a narrow slack does not");
equal(TAIL_SLACK, 48, "the default slack");

// ---- tailTop ----

equal(tailTop(1000, 400), 600, "the tail's scrollTop");
equal(tailTop(400, 400), 0, "a fitting thread scrolls nowhere");
equal(tailTop(200, 400), 0, "a short thread never goes negative");

// ---- jumpLabel ----

equal(jumpLabel(1), "Jump to latest · 1 new", "one arrival");
equal(jumpLabel(7), "Jump to latest · 7 new", "several arrivals");
equal(jumpLabel(0), "Jump to latest", "no count when nothing is unseen");

console.log("homeDock.test.ts: all assertions passed");
