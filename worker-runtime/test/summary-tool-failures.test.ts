import { expect, it } from "bun:test";
import { collectUserToolRestrictions, rewriteGiveUps } from "../src/summary-tool-failures.ts";

const failed = new Set(["canvas_open"]);

it("replaces standalone give-ups about the exact failed tool", () => {
  const text = "canvas_open does not work right now. Avoid calling `canvas_open` until it is fixed. my_canvas_open is broken right now.";
  expect(rewriteGiveUps(text, failed, new Map())).toBe("An earlier canvas_open call failed; try it again normally. An earlier canvas_open call failed; try it again normally. my_canvas_open is broken right now.");
});

it("preserves a user restriction without exempting related publication policy", () => {
  const text = "Avoid calling canvas_open until it is fixed.";
  const restricted = new Map<string, Set<string>>();
  collectUserToolRestrictions("Do not publish the page before checking it with canvas_open.", restricted);
  expect(rewriteGiveUps(text, failed, restricted)).toBe("An earlier canvas_open call failed; try it again normally.");
  collectUserToolRestrictions("Never call canvas_open on the production board.", restricted);
  expect(rewriteGiveUps(`canvas_open does not work right now. ${text}`, failed, restricted)).toBe(
    "An earlier canvas_open call failed; try it again normally. An earlier canvas_open call failed; try it again normally.",
  );
  collectUserToolRestrictions("Don't call\ncanvas_open until fixed.", restricted);
  expect(rewriteGiveUps(text, failed, restricted)).toBe(text);
  expect(rewriteGiveUps("canvas_open does not work right now.", failed, restricted)).toBe(
    "An earlier canvas_open call failed.",
  );
});

it("keeps a paraphrased no-retry instruction and summarizes status without recommending retry", () => {
  const restricted = new Map<string, Set<string>>();
  collectUserToolRestrictions("Do not call canvas_open again.", restricted);
  expect(rewriteGiveUps("Do not retry canvas_open. canvas_open does not work right now.", failed, restricted))
    .toBe("Do not retry canvas_open. An earlier canvas_open call failed.");
});

it("keeps a user restriction on either side of a top-level semicolon", () => {
  const restatement = "Don't call canvas_open until it is fixed.";
  for (const text of [
    "Don't call canvas_open until it is fixed; use the backup dashboard instead.",
    "Use the backup dashboard; don't call `canvas_open` until it is fixed.",
  ]) {
    const restricted = new Map<string, Set<string>>();
    collectUserToolRestrictions(text, restricted);
    expect(rewriteGiveUps(restatement, failed, restricted)).toBe(restatement);
  }
});

it("does not infer restrictions from quoted examples or Markdown code", () => {
  for (const text of [
    "Copy this example: `setup; do not retry canvas_open; wrap up`.",
    'Copy this example: "setup; do not retry canvas_open; wrap up".',
    String.raw`Copy this example: \"setup; do not retry canvas_open; wrap up\".`,
    "Copy this example: ‘Setup; don’t retry; do not retry canvas_open; wrap up’.",
    "Copy this example: 「setup; do not retry canvas_open; wrap up」.",
    "Copy this example: ``setup `value`; do not retry canvas_open; wrap up``.",
    "Example:\n\n```text\nsetup; do not retry canvas_open; wrap up\n```",
    "Example:\n\n~~~text\nsetup; do not retry canvas_open; wrap up\n~~~",
    "Example:\n\n    setup; do not retry canvas_open; wrap up",
    "> setup; do not retry canvas_open; wrap up",
  ]) {
    const restricted = new Map<string, Set<string>>();
    collectUserToolRestrictions(text, restricted);
    expect(rewriteGiveUps("Do not retry canvas_open.", failed, restricted))
      .toBe("An earlier canvas_open call failed; try it again normally.");
  }
});

it("recognizes a real instruction after a quoted block", () => {
  const restricted = new Map<string, Set<string>>();
  collectUserToolRestrictions(
    "Example:\n\n````md\n```\nsetup; do not retry canvas_open; wrap up\n```\n````\nDo not retry `archive_read`.",
    restricted,
  );
  const bothFailed = new Set(["canvas_open", "archive_read"]);
  expect(rewriteGiveUps("Do not retry canvas_open. Do not retry archive_read.", bothFailed, restricted))
    .toBe("An earlier canvas_open call failed; try it again normally. Do not retry archive_read.");
});

it("only rewrites the unquoted claim beside an assistant's example", () => {
  for (const quote of [
    'The guide says: "Run once. Do not retry canvas_open. Use the backup".',
    'The guide says: "Run once. Do not retry canvas_open. Use the backup."\n\n',
    'The tool returned "Temporary failure."',
    'The changelog says, "The staging deploy finished."',
    String.raw`The log reads: \"Temporary failure.\"`,
  ]) {
    const text = `${quote} canvas_open does not work right now.`;
    expect(rewriteGiveUps(text, failed, new Map())).toBe(`${quote} An earlier canvas_open call failed; try it again normally.`);
  }
});

it("recognizes a user restriction in a new sentence after a complete report", () => {
  const restriction = "Do not retry canvas_open.";
  const restricted = new Map<string, Set<string>>();
  collectUserToolRestrictions(`The tool returned "Temporary failure." ${restriction}`, restricted);
  expect(rewriteGiveUps(restriction, failed, restricted)).toBe(restriction);
});

it("never makes the consequence after a quoted label a standalone claim", () => {
  for (const text of [
    'If the board shows "Read only!" do not retry canvas_open.',
    'When the tool reports "Read only." Do not retry canvas_open.',
    'The board only if it shows "Read only!" Do not retry canvas_open.',
  ]) {
    const restricted = new Map<string, Set<string>>();
    collectUserToolRestrictions(text, restricted);
    expect(rewriteGiveUps(text, failed, new Map())).toBe(text);
    expect(rewriteGiveUps("Do not retry canvas_open.", failed, restricted))
      .toBe("An earlier canvas_open call failed; try it again normally.");
  }
});

it("keeps scanning after inch marks and unmatched inline delimiters", () => {
  for (const prefix of ['A 12" display.', 'The literal " mark.', "The literal ` mark."]) {
    const restriction = "Do not retry canvas_open.";
    const text = `${prefix} ${restriction}`;
    const restricted = new Map<string, Set<string>>();
    collectUserToolRestrictions(text, restricted);
    expect(rewriteGiveUps(restriction, failed, restricted)).toBe(restriction);
    expect(rewriteGiveUps(text, failed, new Map())).toBe(`${prefix} An earlier canvas_open call failed; try it again normally.`);
  }
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
  expect(rewriteGiveUps(text, failed, new Map())).toBe(text);
});

it("scans a long assistant message with many near-matches in bounded time", () => {
  const text = "canvas_open is broken until fixed ".repeat(20_000);
  const started = performance.now();
  expect(rewriteGiveUps(text, failed, new Map())).toBe(text);
  expect(performance.now() - started).toBeLessThan(500);
});
