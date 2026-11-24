import { expect, it } from "bun:test";
import { collectUserToolRestrictions, rewriteGiveUps } from "../src/summary-tool-failures.ts";

const failed = new Set(["canvas_open"]);

it("replaces standalone give-ups about the exact failed tool", () => {
  const text = "canvas_open does not work right now. Avoid calling `canvas_open` until it is fixed. my_canvas_open is broken right now.";
  expect(rewriteGiveUps(text, failed, new Set())).toBe("An earlier canvas_open call failed; try it again normally. An earlier canvas_open call failed; try it again normally. my_canvas_open is broken right now.");
});

it("preserves a user restriction without exempting related publication policy", () => {
  const text = "Avoid calling canvas_open until it is fixed.";
  const restricted = new Set<string>();
  collectUserToolRestrictions("Do not publish the page before checking it with canvas_open.", restricted);
  expect(rewriteGiveUps(text, failed, restricted)).toBe("An earlier canvas_open call failed; try it again normally.");
  collectUserToolRestrictions("Never call canvas_open on the production board.", restricted);
  expect(rewriteGiveUps(`canvas_open does not work right now. ${text}`, failed, restricted)).toBe(
    "An earlier canvas_open call failed; try it again normally. An earlier canvas_open call failed; try it again normally.",
  );
  collectUserToolRestrictions("Don't call\ncanvas_open until fixed.", restricted);
  expect(rewriteGiveUps(text, failed, restricted)).toBe(text);
  expect(rewriteGiveUps("canvas_open does not work right now.", failed, restricted)).toBe(
    "An earlier canvas_open call failed; try it again normally.",
  );
});

it("keeps corrections, facts, and scoped limits after a failed call", () => {
  const text = [
    "Stop using the old slug q9 and pass slug q10 to canvas_open.",
    "Don't run the migration yet; canvas_open will show draft board 7 once you approve.",
    "Don't open the archived board 12 with canvas_open, it would unarchive it.",
    "Never try to call canvas_open with a board id from another workspace, ids are scoped.",
    "canvas_open refuses archived boards by design, so I picked board 8.",
    "I'll keep the draft in board 7 until the import is working; canvas_open can show it.",
    "canvas_open is unavailable for archived boards.",
    "canvas_open does not work right now; the draft is saved in board 7.",
    "Don't call canvas_open\non the production board.",
    "canvas_open failed once. The headline uses no emoji.",
  ].join("\n");
  expect(rewriteGiveUps(text, failed, new Set())).toBe(text);
});

it("scans a long assistant message with many near-matches in bounded time", () => {
  const text = "canvas_open is broken until fixed ".repeat(20_000);
  const started = performance.now();
  expect(rewriteGiveUps(text, failed, new Set())).toBe(text);
  expect(performance.now() - started).toBeLessThan(500);
});
